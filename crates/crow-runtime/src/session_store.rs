//! Workspace-namespaced session persistence (Claw-Code pattern).
//!
//! Provides a `WorkspaceSessionStore` that partitions session files by
//! workspace fingerprint, so parallel crow instances in different worktrees
//! never collide. Sessions are stored as JSONL files for incremental
//! append-only writes.
//!
//! # Directory Layout
//!
//! ```text
//! ~/.crow/sessions/<workspace_fingerprint>/
//!   ├── <session_id>.jsonl      // active sessions
//!   └── <session_id>.jsonl.bak  // rotation backups
//! ```
//!
//! # Key Design Decisions
//!
//! - **JSONL format**: Each message is a single JSON line, enabling
//!   incremental appends without rewriting the entire file.
//! - **FNV-1a fingerprinting**: Canonical workspace path is hashed to a
//!   16-char hex string for stable directory partitioning.
//! - **Fork support**: Sessions can be forked into new IDs with lineage
//!   metadata preserved.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Primary file extension for session files.
const SESSION_EXTENSION: &str = "jsonl";

/// Maximum session file size before rotation (256 KB).
const MAX_SESSION_BYTES: u64 = 256 * 1024;

/// Maximum number of rotated backup files to keep.
const MAX_BACKUPS: usize = 3;

/// Session reference aliases (claw-code pattern).
const SESSION_ALIASES: &[&str] = &["latest", "last", "recent"];

// ─── Workspace Fingerprint ──────────────────────────────────────────

/// Stable hex fingerprint of a workspace path using FNV-1a (64-bit).
///
/// Produces a 16-char hex string that partitions the on-disk session
/// directory per workspace root. Identical to claw-code's implementation.
#[must_use]
pub fn workspace_fingerprint(workspace_root: &Path) -> String {
    let input = workspace_root.to_string_lossy();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

// ─── Session Store ──────────────────────────────────────────────────

/// Per-worktree session store that namespaces on-disk session files by
/// workspace fingerprint.
#[derive(Debug, Clone)]
pub struct WorkspaceSessionStore {
    /// Resolved root of the session namespace.
    sessions_root: PathBuf,
    /// The canonical workspace path that was fingerprinted.
    workspace_root: PathBuf,
}

impl WorkspaceSessionStore {
    /// Build a store from the workspace root directory.
    ///
    /// Canonicalizes the workspace path so equivalent paths (symlinks,
    /// relative vs absolute) produce the same fingerprint.
    pub fn from_workspace(workspace_root: &Path) -> Result<Self> {
        let canonical =
            fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        let sessions_root = home_dir()?
            .join(".crow")
            .join("sessions")
            .join(workspace_fingerprint(&canonical));
        fs::create_dir_all(&sessions_root).with_context(|| {
            format!("Failed to create session dir: {}", sessions_root.display())
        })?;
        Ok(Self {
            sessions_root,
            workspace_root: canonical,
        })
    }

    /// The fully resolved sessions directory for this workspace.
    #[must_use]
    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_root
    }

    /// The workspace root this store is bound to.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Save a session record by appending a JSONL entry.
    pub fn append_entry(&self, session_id: &str, entry: &SessionEntry) -> Result<()> {
        let path = self.session_path(session_id);
        self.rotate_if_needed(&path)?;

        let line = serde_json::to_string(entry).context("Failed to serialize session entry")?;

        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Failed to open session file: {}", path.display()))?;

        writeln!(file, "{line}")
            .with_context(|| format!("Failed to append to session file: {}", path.display()))?;

        Ok(())
    }

    /// Load all entries from a session file.
    pub fn load_entries(&self, session_id: &str) -> Result<Vec<SessionEntry>> {
        let path = self.session_path(session_id);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Session not found: {}", path.display()))?;

        let mut entries = Vec::new();
        for (line_num, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: SessionEntry = serde_json::from_str(line)
                .with_context(|| format!("Parse error at line {}: {line}", line_num + 1))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// List all sessions in this workspace namespace, newest first.
    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let mut summaries = Vec::new();

        let entries = match fs::read_dir(&self.sessions_root) {
            Ok(e) => e,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(summaries),
            Err(err) => return Err(err.into()),
        };

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|e| e.to_str()) != Some(SESSION_EXTENSION) {
                continue;
            }

            let metadata = entry.metadata()?;
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or_default();

            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            // Read first and last lines for summary
            let (first_prompt, entry_count) = self.peek_session_summary(&path);

            summaries.push(SessionSummary {
                id: session_id,
                modified_ms,
                first_prompt,
                entry_count,
                path,
            });
        }

        // Sort newest first
        summaries.sort_by_key(|s| std::cmp::Reverse(s.modified_ms));
        Ok(summaries)
    }

    /// Resolve a session reference ("latest", "last", or a session ID).
    pub fn resolve_reference(&self, reference: &str) -> Result<String> {
        if is_alias(reference) {
            let sessions = self.list_sessions()?;
            sessions
                .first()
                .map(|s| s.id.clone())
                .ok_or_else(|| anyhow::anyhow!("No sessions found in this workspace"))
        } else {
            // Verify the session exists
            let path = self.session_path(reference);
            if path.exists() {
                Ok(reference.to_string())
            } else {
                Err(anyhow::anyhow!("Session not found: {reference}"))
            }
        }
    }

    // ── Internal ────────────────────────────────────────────────────

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_root
            .join(format!("{session_id}.{SESSION_EXTENSION}"))
    }

    fn rotate_if_needed(&self, path: &Path) -> Result<()> {
        let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size < MAX_SESSION_BYTES {
            return Ok(());
        }

        // Rotate: .jsonl → .jsonl.1, .jsonl.1 → .jsonl.2, etc.
        for i in (1..MAX_BACKUPS).rev() {
            let from = path.with_extension(format!("{SESSION_EXTENSION}.{i}"));
            let to = path.with_extension(format!("{SESSION_EXTENSION}.{}", i + 1));
            if from.exists() {
                let _ = fs::rename(&from, &to);
            }
        }
        let backup = path.with_extension(format!("{SESSION_EXTENSION}.1"));
        let _ = fs::rename(path, &backup);

        Ok(())
    }

    fn peek_session_summary(&self, path: &Path) -> (Option<String>, usize) {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return (None, 0),
        };

        let mut first_prompt = None;
        let mut count = 0;

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            count += 1;
            if first_prompt.is_none() {
                if let Ok(entry) = serde_json::from_str::<SessionEntry>(line) {
                    if entry.role == "user" {
                        let preview = if entry.content.len() > 60 {
                            format!("{}…", &entry.content[..57])
                        } else {
                            entry.content.clone()
                        };
                        first_prompt = Some(preview);
                    }
                }
            }
        }

        (first_prompt, count)
    }
}

// ─── Session Entry ──────────────────────────────────────────────────

/// A single JSONL entry in a session file.
///
/// Each entry represents one message in the conversation. Tool calls
/// and tool results are serialized as JSON strings in the `content` field
/// with appropriate role markers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    /// Message role: "system", "user", "assistant", "tool".
    pub role: String,
    /// Message content.
    pub content: String,
    /// Unix timestamp (seconds since epoch).
    #[serde(default)]
    pub timestamp: u64,
    /// Optional tool call ID (for tool role messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional metadata (compaction markers, fork info, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<String>,
}

impl SessionEntry {
    /// Create a new entry with the current timestamp.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            timestamp: unix_now(),
            tool_call_id: None,
            meta: None,
        }
    }

    /// Create a tool result entry.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            timestamp: unix_now(),
            tool_call_id: Some(tool_call_id.into()),
            meta: None,
        }
    }

    /// Create a metadata-only entry (compaction markers, etc.)
    pub fn meta(kind: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "meta".into(),
            content: content.into(),
            timestamp: unix_now(),
            tool_call_id: None,
            meta: Some(kind.into()),
        }
    }
}

// ─── Session Summary ────────────────────────────────────────────────

/// Lightweight session metadata for listing.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub modified_ms: u64,
    pub first_prompt: Option<String>,
    pub entry_count: usize,
    pub path: PathBuf,
}

impl std::fmt::Display for SessionSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let id_short = &self.id[..8.min(self.id.len())];
        let prompt = self.first_prompt.as_deref().unwrap_or("(empty)");
        write!(f, "  {id_short} │ {prompt} │ {} entries", self.entry_count)
    }
}

// ─── Utilities ──────────────────────────────────────────────────────

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))
}

fn is_alias(reference: &str) -> bool {
    SESSION_ALIASES
        .iter()
        .any(|alias| reference.eq_ignore_ascii_case(alias))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn workspace_fingerprint_is_deterministic() {
        let p = Path::new("/tmp/test-workspace");
        let fp1 = workspace_fingerprint(p);
        let fp2 = workspace_fingerprint(p);
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
    }

    #[test]
    fn workspace_fingerprint_differs_per_path() {
        let fp_a = workspace_fingerprint(Path::new("/tmp/alpha"));
        let fp_b = workspace_fingerprint(Path::new("/tmp/beta"));
        assert_ne!(fp_a, fp_b);
    }

    #[test]
    fn session_entry_roundtrip() {
        let entry = SessionEntry::new("user", "hello world");
        let json = serde_json::to_string(&entry).unwrap();
        let restored: SessionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.role, "user");
        assert_eq!(restored.content, "hello world");
        assert!(restored.timestamp > 0);
    }

    #[test]
    fn session_store_append_and_load() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkspaceSessionStore {
            sessions_root: tmp.path().to_path_buf(),
            workspace_root: tmp.path().to_path_buf(),
        };

        let sid = "test-session-001";
        store
            .append_entry(sid, &SessionEntry::new("user", "first message"))
            .unwrap();
        store
            .append_entry(sid, &SessionEntry::new("assistant", "response"))
            .unwrap();

        let entries = store.load_entries(sid).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[1].role, "assistant");
    }

    #[test]
    fn session_store_list_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = WorkspaceSessionStore {
            sessions_root: tmp.path().to_path_buf(),
            workspace_root: tmp.path().to_path_buf(),
        };

        store
            .append_entry("sess-a", &SessionEntry::new("user", "task alpha"))
            .unwrap();
        store
            .append_entry("sess-b", &SessionEntry::new("user", "task beta"))
            .unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn session_aliases_resolve() {
        assert!(is_alias("latest"));
        assert!(is_alias("LAST"));
        assert!(is_alias("Recent"));
        assert!(!is_alias("sess-abc"));
    }

    #[test]
    fn session_summary_display() {
        let summary = SessionSummary {
            id: "abcdef1234567890".into(),
            modified_ms: 0,
            first_prompt: Some("fix the bug".into()),
            entry_count: 5,
            path: PathBuf::from("/tmp/test.jsonl"),
        };
        let display = format!("{summary}");
        assert!(display.contains("abcdef12"));
        assert!(display.contains("fix the bug"));
    }
}
