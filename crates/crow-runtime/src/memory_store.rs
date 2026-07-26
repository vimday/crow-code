//! Simplified Dynamic Memories for Crow Code.
//!
//! Persists session learnings (key decisions, patterns, summaries) to
//! `~/.crow/memories/` as individual JSON files keyed by session ID.
//! Memories are ranked by usage count and recency for injection into
//! future system prompts.
//!
//! # Directory Layout
//!
//! ```text
//! ~/.crow/memories/
//!   ├── <session_id>.json
//!   └── <session_id>.json
//! ```

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

// ─── Types ──────────────────────────────────────────────────────────

/// A single memory entry captured from a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// High-level summary of what happened during the session.
    pub summary: String,
    /// Concrete decisions made (e.g. "chose RwLock over Mutex for X").
    pub key_decisions: Vec<String>,
    /// Reusable patterns discovered (e.g. "always inline format args").
    pub patterns_learned: Vec<String>,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
    /// Number of times this memory has been referenced.
    pub usage_count: u32,
    /// Unique identifier tying this memory to its originating session.
    pub session_id: String,
}

/// Manages the on-disk `~/.crow/memories/` directory.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    memories_dir: PathBuf,
}

// ─── Constants ──────────────────────────────────────────────────────

/// Subdirectory under `~/.crow/` where memories live.
const MEMORIES_SUBDIR: &str = "memories";

/// File extension for memory files.
const MEMORY_EXT: &str = "json";

/// Rough chars-per-token estimate used for budget capping.
const CHARS_PER_TOKEN: usize = 4;

// ─── MemoryStore ────────────────────────────────────────────────────

impl MemoryStore {
    /// Create a new `MemoryStore` at the default location (`~/.crow/memories/`).
    ///
    /// Creates the directory tree if it does not yet exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined or the
    /// directory cannot be created.
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        let memories_dir = home.join(".crow").join(MEMORIES_SUBDIR);
        Self::with_path(memories_dir)
    }

    /// Create a `MemoryStore` at a custom path (useful for testing).
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn with_path(memories_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&memories_dir).with_context(|| {
            format!("failed to create memories dir: {}", memories_dir.display())
        })?;
        Ok(Self { memories_dir })
    }

    /// Returns the path to the memories directory.
    #[must_use]
    pub fn memories_dir(&self) -> &Path {
        &self.memories_dir
    }

    /// Persist a [`MemoryEntry`] to disk as `{session_id}.json`.
    ///
    /// Overwrites any existing file for the same session ID.
    ///
    /// # Errors
    ///
    /// Returns an error if serialisation or file I/O fails.
    pub fn save_memory(&self, entry: &MemoryEntry) -> Result<()> {
        let path = self.memory_path(&entry.session_id);
        let json =
            serde_json::to_string_pretty(entry).context("failed to serialise memory entry")?;
        fs::write(&path, json)
            .with_context(|| format!("failed to write memory file: {}", path.display()))?;
        tracing::debug!("saved memory for session {}", entry.session_id);
        Ok(())
    }

    /// Load every memory on disk, sorted by `usage_count` descending then
    /// `created_at` descending (most-used and most-recent first).
    ///
    /// Malformed files are logged and skipped rather than failing the
    /// entire load.
    ///
    /// # Errors
    ///
    /// Returns an error only if the memories directory cannot be read.
    pub fn load_all_memories(&self) -> Result<Vec<MemoryEntry>> {
        let mut entries = Vec::new();

        let dir_iter = fs::read_dir(&self.memories_dir).with_context(|| {
            format!(
                "failed to read memories dir: {}",
                self.memories_dir.display()
            )
        })?;

        for dir_entry in dir_iter {
            let dir_entry = match dir_entry {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!("skipping unreadable dir entry: {err}");
                    continue;
                }
            };

            let path = dir_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(MEMORY_EXT) {
                continue;
            }

            match self.load_single(&path) {
                Ok(entry) => entries.push(entry),
                Err(err) => {
                    tracing::warn!("skipping malformed memory file {}: {err}", path.display());
                }
            }
        }

        // Sort: highest usage_count first, then newest created_at first.
        entries.sort_by(|a, b| {
            b.usage_count
                .cmp(&a.usage_count)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });

        tracing::debug!("loaded {} memories", entries.len());
        Ok(entries)
    }

    /// Build a markdown-formatted string of the top memories suitable for
    /// injection into a system prompt.
    ///
    /// Returns at most `max_entries` memories and stays within an
    /// approximate `max_tokens` budget (estimated at ~4 chars per token).
    ///
    /// # Errors
    ///
    /// Propagates errors from [`Self::load_all_memories`].
    pub fn load_memories_for_prompt(
        &self,
        max_entries: usize,
        max_tokens: usize,
    ) -> Result<String> {
        let entries = self.load_all_memories()?;
        let max_chars = max_tokens.saturating_mul(CHARS_PER_TOKEN);
        let mut output = String::with_capacity(max_chars.min(4096));
        let mut count = 0usize;

        for entry in entries.iter().take(max_entries) {
            let section = format_memory_section(entry);
            if output.len() + section.len() > max_chars {
                break;
            }
            output.push_str(&section);
            output.push('\n');
            count += 1;
        }

        tracing::debug!(
            "formatted {count} memories for prompt ({} chars, budget {max_chars})",
            output.len(),
        );
        Ok(output)
    }

    /// Increment the `usage_count` of a memory identified by session ID.
    ///
    /// No-ops (with a debug log) if the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or writing the file fails.
    pub fn record_usage(&self, session_id: &str) -> Result<()> {
        let path = self.memory_path(session_id);
        if !path.exists() {
            tracing::debug!("no memory file for session {session_id}, skipping usage bump");
            return Ok(());
        }

        let mut entry: MemoryEntry = self
            .load_single(&path)
            .with_context(|| format!("failed to load memory for usage bump: {session_id}"))?;

        entry.usage_count = entry.usage_count.saturating_add(1);
        self.save_memory(&entry)?;
        tracing::debug!(
            "bumped usage count for session {session_id} to {}",
            entry.usage_count
        );
        Ok(())
    }

    /// Delete memories older than `max_age_days`.
    ///
    /// Returns the number of files removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read. Individual
    /// deletion failures are logged and skipped.
    pub fn prune_old_memories(&self, max_age_days: u64) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::days(max_age_days as i64);
        let entries = self.load_all_memories()?;
        let mut pruned = 0usize;

        for entry in &entries {
            let created = match entry.created_at.parse::<DateTime<Utc>>() {
                Ok(dt) => dt,
                Err(err) => {
                    tracing::warn!(
                        "cannot parse created_at for session {}: {err}",
                        entry.session_id,
                    );
                    continue;
                }
            };

            if created < cutoff {
                let path = self.memory_path(&entry.session_id);
                if let Err(err) = fs::remove_file(&path) {
                    tracing::warn!("failed to prune {}: {err}", path.display());
                } else {
                    pruned += 1;
                    tracing::debug!("pruned old memory: {}", entry.session_id);
                }
            }
        }

        tracing::info!("pruned {pruned} memories older than {max_age_days} days");
        Ok(pruned)
    }

    // ── Private helpers ─────────────────────────────────────────────

    /// Canonical file path for a given session ID.
    fn memory_path(&self, session_id: &str) -> PathBuf {
        self.memories_dir.join(format!("{session_id}.{MEMORY_EXT}"))
    }

    /// Deserialise a single memory file.
    fn load_single(&self, path: &Path) -> Result<MemoryEntry> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let entry: MemoryEntry = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(entry)
    }
}

// ─── Formatting ─────────────────────────────────────────────────────

/// Format a single [`MemoryEntry`] as a markdown section.
///
/// ```text
/// ## Session: <session_id>
/// <summary>
///
/// **Key decisions:**
/// - decision 1
/// - decision 2
///
/// **Patterns learned:**
/// - pattern 1
/// ```
#[must_use]
pub fn format_memory_section(entry: &MemoryEntry) -> String {
    let mut out = format!("## Session: {}\n{}\n", entry.session_id, entry.summary);

    if !entry.key_decisions.is_empty() {
        out.push_str("\n**Key decisions:**\n");
        for decision in &entry.key_decisions {
            out.push_str(&format!("- {decision}\n"));
        }
    }

    if !entry.patterns_learned.is_empty() {
        out.push_str("\n**Patterns learned:**\n");
        for pattern in &entry.patterns_learned {
            out.push_str(&format!("- {pattern}\n"));
        }
    }

    out
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn sample_entry(session_id: &str) -> MemoryEntry {
        MemoryEntry {
            summary: format!("Session {session_id} summary"),
            key_decisions: vec!["chose RwLock".into(), "used builder pattern".into()],
            patterns_learned: vec!["inline format args".into()],
            created_at: Utc::now().to_rfc3339(),
            usage_count: 0,
            session_id: session_id.to_string(),
        }
    }

    #[test]
    fn round_trip_save_and_load() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = MemoryStore::with_path(tmp.path().to_path_buf()).expect("store");

        let entry = sample_entry("test-001");
        store.save_memory(&entry).expect("save");

        let loaded = store.load_all_memories().expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].session_id, "test-001");
        assert_eq!(loaded[0].key_decisions.len(), 2);
    }

    #[test]
    fn usage_count_increments() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = MemoryStore::with_path(tmp.path().to_path_buf()).expect("store");

        let entry = sample_entry("test-002");
        store.save_memory(&entry).expect("save");

        store.record_usage("test-002").expect("record");
        store.record_usage("test-002").expect("record");

        let loaded = store.load_all_memories().expect("load");
        assert_eq!(loaded[0].usage_count, 2);
    }

    #[test]
    fn sort_order_usage_then_date() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = MemoryStore::with_path(tmp.path().to_path_buf()).expect("store");

        let mut a = sample_entry("aaa");
        a.usage_count = 5;
        a.created_at = "2025-01-01T00:00:00Z".into();

        let mut b = sample_entry("bbb");
        b.usage_count = 5;
        b.created_at = "2025-06-01T00:00:00Z".into();

        let mut c = sample_entry("ccc");
        c.usage_count = 10;

        store.save_memory(&a).expect("save a");
        store.save_memory(&b).expect("save b");
        store.save_memory(&c).expect("save c");

        let loaded = store.load_all_memories().expect("load");
        assert_eq!(loaded[0].session_id, "ccc"); // highest usage
        assert_eq!(loaded[1].session_id, "bbb"); // same usage as aaa but newer
        assert_eq!(loaded[2].session_id, "aaa");
    }

    #[test]
    fn prompt_respects_token_budget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = MemoryStore::with_path(tmp.path().to_path_buf()).expect("store");

        for i in 0..20 {
            let entry = sample_entry(&format!("session-{i:03}"));
            store.save_memory(&entry).expect("save");
        }

        // Tiny budget: should only fit a few.
        let prompt = store.load_memories_for_prompt(100, 50).expect("prompt");
        assert!(!prompt.is_empty());
        assert!(prompt.len() <= 50 * CHARS_PER_TOKEN);
    }

    #[test]
    fn prune_removes_old_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = MemoryStore::with_path(tmp.path().to_path_buf()).expect("store");

        let mut old = sample_entry("old-session");
        old.created_at = "2020-01-01T00:00:00Z".into();
        store.save_memory(&old).expect("save");

        let fresh = sample_entry("fresh-session");
        store.save_memory(&fresh).expect("save");

        let pruned = store.prune_old_memories(365).expect("prune");
        assert_eq!(pruned, 1);

        let remaining = store.load_all_memories().expect("load");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].session_id, "fresh-session");
    }

    #[test]
    fn format_memory_section_output() {
        let entry = sample_entry("fmt-test");
        let section = format_memory_section(&entry);
        assert!(section.contains("## Session: fmt-test"));
        assert!(section.contains("chose RwLock"));
        assert!(section.contains("inline format args"));
    }

    #[test]
    fn record_usage_noop_for_missing_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = MemoryStore::with_path(tmp.path().to_path_buf()).expect("store");
        // Should not error, just no-op.
        store.record_usage("nonexistent").expect("noop");
    }
}
