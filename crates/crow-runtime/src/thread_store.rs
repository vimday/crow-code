//! Thread-based session persistence for Crow Code.
//!
//! Provides a trait-based abstraction ([`ThreadStore`]) over conversation
//! thread storage, allowing pluggable backends (in-memory, SQLite, etc.)
//! without coupling to any specific database crate.
//!
//! # Data Model
//!
//! ```text
//! Thread (1) ──< (N) Turn ──< (N) ToolCallRecord
//! ```
//!
//! Each [`Thread`] represents a conversation session. A [`Turn`] captures
//! a single user prompt / assistant response pair, along with tool-call
//! records and token usage metadata. [`ThreadStats`] provides aggregated
//! metrics for a thread.

use std::hash::{BuildHasher, Hasher};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

// ─── Types ──────────────────────────────────────────────────────────

/// A conversation thread (session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    /// Unique identifier for this thread.
    pub id: String,
    /// Optional human-readable title (often derived from the first prompt).
    pub title: Option<String>,
    /// Model identifier used for this session (e.g. `"claude-sonnet-4-6"`).
    pub model: String,
    /// Workspace path associated with this thread.
    pub workspace: String,
    /// When the thread was first created.
    pub created_at: SystemTime,
    /// When the thread was last modified (turn added, metadata changed).
    pub updated_at: SystemTime,
    /// Number of turns in this thread.
    pub turn_count: u32,
    /// Cumulative input tokens across all turns.
    pub total_input_tokens: u64,
    /// Cumulative output tokens across all turns.
    pub total_output_tokens: u64,
    /// Whether this thread has been archived (hidden from default listings).
    pub is_archived: bool,
}

/// A single turn within a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    /// Unique identifier for this turn.
    pub id: String,
    /// Parent thread ID.
    pub thread_id: String,
    /// Zero-based index within the thread.
    pub index: u32,
    /// The user's prompt text.
    pub user_prompt: String,
    /// The assistant's response text.
    pub assistant_response: String,
    /// Number of input tokens consumed.
    pub input_tokens: u32,
    /// Number of output tokens generated.
    pub output_tokens: u32,
    /// Tool calls made during this turn.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Wall-clock duration of this turn in milliseconds.
    pub duration_ms: u64,
    /// When this turn was created.
    pub created_at: SystemTime,
}

/// Record of a tool call within a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Tool name (e.g. `"read_file"`, `"grep_search"`).
    pub name: String,
    /// JSON-serialised arguments passed to the tool.
    pub arguments: String,
    /// Result returned by the tool (may be truncated).
    pub result: String,
    /// Duration of the tool call in milliseconds.
    pub duration_ms: u64,
    /// Whether the user approved the tool call (relevant for write tools).
    pub was_approved: bool,
}

/// Aggregated statistics for a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadStats {
    /// Total number of turns.
    pub total_turns: u32,
    /// Total input tokens across all turns.
    pub total_input_tokens: u64,
    /// Total output tokens across all turns.
    pub total_output_tokens: u64,
    /// Total number of tool calls across all turns.
    pub total_tool_calls: u32,
    /// Total wall-clock duration across all turns in milliseconds.
    pub total_duration_ms: u64,
    /// Estimated cost in USD (rough heuristic).
    pub estimated_cost_usd: f64,
}

// ─── Trait ───────────────────────────────────────────────────────────

/// Abstract thread store trait.
///
/// Implementations must be `Send + Sync` to support concurrent access
/// from the TUI and agent loops.
pub trait ThreadStore: Send + Sync {
    /// Create or update a thread.
    fn save_thread(&self, thread: &Thread) -> anyhow::Result<()>;

    /// Get a thread by ID. Returns `None` if not found.
    fn get_thread(&self, id: &str) -> anyhow::Result<Option<Thread>>;

    /// List non-archived threads ordered by `updated_at` descending.
    fn list_threads(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<Thread>>;

    /// Archive a thread (soft-delete). Returns an error if the thread
    /// does not exist.
    fn archive_thread(&self, id: &str) -> anyhow::Result<()>;

    /// Permanently delete a thread and all of its turns. Returns an error
    /// if the thread does not exist.
    fn delete_thread(&self, id: &str) -> anyhow::Result<()>;

    /// Save a turn, appending it to its parent thread.
    fn save_turn(&self, turn: &Turn) -> anyhow::Result<()>;

    /// Get turns for a thread ordered by index ascending, with pagination.
    fn get_turns(&self, thread_id: &str, limit: usize, offset: usize) -> anyhow::Result<Vec<Turn>>;

    /// Get the most recent turn for a thread.
    fn last_turn(&self, thread_id: &str) -> anyhow::Result<Option<Turn>>;

    /// Compute aggregated statistics for a thread.
    fn thread_stats(&self, thread_id: &str) -> anyhow::Result<ThreadStats>;
}

// ─── ID Generation ──────────────────────────────────────────────────

/// Generate a pseudo-random `u64` without depending on the `rand` crate.
///
/// Uses `RandomState` from the standard library, which is seeded with
/// per-process entropy, to produce a hash-based pseudo-random value.
fn pseudo_random_u64() -> u64 {
    let state = std::collections::hash_map::RandomState::new();
    let mut hasher = state.build_hasher();
    // Hash the current time for additional entropy per call.
    hasher.write_u128(
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    hasher.finish()
}

/// Generate a unique thread ID.
///
/// Format: `thread_{timestamp_ms}_{random_hex}`
#[must_use]
pub fn generate_thread_id() -> String {
    let ts = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let rand_suffix = pseudo_random_u64() as u32;
    format!("thread_{ts}_{rand_suffix:08x}")
}

/// Generate a unique turn ID.
///
/// Format: `turn_{timestamp_ms}_{random_hex}`
#[must_use]
pub fn generate_turn_id() -> String {
    let ts = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let rand_suffix = pseudo_random_u64() as u32;
    format!("turn_{ts}_{rand_suffix:08x}")
}

// ─── In-Memory Implementation ───────────────────────────────────────

/// Estimated USD cost per million input tokens (rough average across providers).
const INPUT_COST_PER_M: f64 = 3.0;
/// Estimated USD cost per million output tokens.
const OUTPUT_COST_PER_M: f64 = 15.0;

/// In-memory implementation of [`ThreadStore`].
///
/// Suitable for testing, short-lived CLI sessions, and as a reference
/// implementation for the trait contract.
#[derive(Debug, Default)]
pub struct InMemoryThreadStore {
    threads: std::sync::Mutex<Vec<Thread>>,
    turns: std::sync::Mutex<Vec<Turn>>,
}

impl InMemoryThreadStore {
    /// Create a new empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ThreadStore for InMemoryThreadStore {
    fn save_thread(&self, thread: &Thread) -> anyhow::Result<()> {
        let mut threads = self
            .threads
            .lock()
            .map_err(|e| anyhow::anyhow!("thread lock poisoned: {e}"))?;

        if let Some(existing) = threads.iter_mut().find(|t| t.id == thread.id) {
            *existing = thread.clone();
        } else {
            threads.push(thread.clone());
        }
        Ok(())
    }

    fn get_thread(&self, id: &str) -> anyhow::Result<Option<Thread>> {
        let threads = self
            .threads
            .lock()
            .map_err(|e| anyhow::anyhow!("thread lock poisoned: {e}"))?;

        Ok(threads.iter().find(|t| t.id == id).cloned())
    }

    fn list_threads(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<Thread>> {
        let threads = self
            .threads
            .lock()
            .map_err(|e| anyhow::anyhow!("thread lock poisoned: {e}"))?;

        let mut active: Vec<_> = threads.iter().filter(|t| !t.is_archived).cloned().collect();
        // Most recently updated first.
        active.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
        Ok(active.into_iter().skip(offset).take(limit).collect())
    }

    fn archive_thread(&self, id: &str) -> anyhow::Result<()> {
        let mut threads = self
            .threads
            .lock()
            .map_err(|e| anyhow::anyhow!("thread lock poisoned: {e}"))?;

        let thread = threads
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow::anyhow!("thread not found: {id}"))?;
        thread.is_archived = true;
        Ok(())
    }

    fn delete_thread(&self, id: &str) -> anyhow::Result<()> {
        let mut threads = self
            .threads
            .lock()
            .map_err(|e| anyhow::anyhow!("thread lock poisoned: {e}"))?;

        let len_before = threads.len();
        threads.retain(|t| t.id != id);
        if threads.len() == len_before {
            return Err(anyhow::anyhow!("thread not found: {id}"));
        }

        // Also remove associated turns.
        let mut turns = self
            .turns
            .lock()
            .map_err(|e| anyhow::anyhow!("turn lock poisoned: {e}"))?;
        turns.retain(|t| t.thread_id != id);
        Ok(())
    }

    fn save_turn(&self, turn: &Turn) -> anyhow::Result<()> {
        let mut turns = self
            .turns
            .lock()
            .map_err(|e| anyhow::anyhow!("turn lock poisoned: {e}"))?;

        if let Some(existing) = turns.iter_mut().find(|t| t.id == turn.id) {
            *existing = turn.clone();
        } else {
            turns.push(turn.clone());
        }
        Ok(())
    }

    fn get_turns(&self, thread_id: &str, limit: usize, offset: usize) -> anyhow::Result<Vec<Turn>> {
        let turns = self
            .turns
            .lock()
            .map_err(|e| anyhow::anyhow!("turn lock poisoned: {e}"))?;

        let mut matched: Vec<_> = turns
            .iter()
            .filter(|t| t.thread_id == thread_id)
            .cloned()
            .collect();
        matched.sort_by_key(|t| t.index);
        Ok(matched.into_iter().skip(offset).take(limit).collect())
    }

    fn last_turn(&self, thread_id: &str) -> anyhow::Result<Option<Turn>> {
        let turns = self
            .turns
            .lock()
            .map_err(|e| anyhow::anyhow!("turn lock poisoned: {e}"))?;

        Ok(turns
            .iter()
            .filter(|t| t.thread_id == thread_id)
            .max_by_key(|t| t.index)
            .cloned())
    }

    fn thread_stats(&self, thread_id: &str) -> anyhow::Result<ThreadStats> {
        let turns = self
            .turns
            .lock()
            .map_err(|e| anyhow::anyhow!("turn lock poisoned: {e}"))?;

        let matching: Vec<_> = turns.iter().filter(|t| t.thread_id == thread_id).collect();

        let total_turns = matching.len() as u32;
        let total_input_tokens: u64 = matching.iter().map(|t| u64::from(t.input_tokens)).sum();
        let total_output_tokens: u64 = matching.iter().map(|t| u64::from(t.output_tokens)).sum();
        let total_tool_calls: u32 = matching.iter().map(|t| t.tool_calls.len() as u32).sum();
        let total_duration_ms: u64 = matching.iter().map(|t| t.duration_ms).sum();

        let estimated_cost_usd = (total_input_tokens as f64 / 1_000_000.0) * INPUT_COST_PER_M
            + (total_output_tokens as f64 / 1_000_000.0) * OUTPUT_COST_PER_M;

        Ok(ThreadStats {
            total_turns,
            total_input_tokens,
            total_output_tokens,
            total_tool_calls,
            total_duration_ms,
            estimated_cost_usd,
        })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn make_thread(id: &str, updated_at: SystemTime) -> Thread {
        Thread {
            id: id.to_string(),
            title: Some(format!("Thread {id}")),
            model: "test-model".to_string(),
            workspace: "/tmp/test".to_string(),
            created_at: SystemTime::now(),
            updated_at,
            turn_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            is_archived: false,
        }
    }

    fn make_turn(id: &str, thread_id: &str, index: u32) -> Turn {
        Turn {
            id: id.to_string(),
            thread_id: thread_id.to_string(),
            index,
            user_prompt: format!("prompt {index}"),
            assistant_response: format!("response {index}"),
            input_tokens: 100 * (index + 1),
            output_tokens: 200 * (index + 1),
            tool_calls: vec![ToolCallRecord {
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
                result: "ok".to_string(),
                duration_ms: 50,
                was_approved: true,
            }],
            duration_ms: 1000 * u64::from(index + 1),
            created_at: SystemTime::now(),
        }
    }

    #[test]
    fn create_and_retrieve_thread() {
        let store = InMemoryThreadStore::new();
        let t = make_thread("t1", SystemTime::now());
        store.save_thread(&t).unwrap();

        let loaded = store.get_thread("t1").unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.id, "t1");
        assert_eq!(loaded.title.as_deref(), Some("Thread t1"));

        // Non-existent thread returns None.
        assert!(store.get_thread("nonexistent").unwrap().is_none());
    }

    #[test]
    fn list_threads_returns_most_recent_first() {
        let store = InMemoryThreadStore::new();
        let now = SystemTime::now();
        let earlier = now - Duration::from_secs(60);
        let later = now + Duration::from_secs(60);

        store.save_thread(&make_thread("old", earlier)).unwrap();
        store.save_thread(&make_thread("mid", now)).unwrap();
        store.save_thread(&make_thread("new", later)).unwrap();

        let listed = store.list_threads(10, 0).unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].id, "new");
        assert_eq!(listed[1].id, "mid");
        assert_eq!(listed[2].id, "old");
    }

    #[test]
    fn list_threads_pagination() {
        let store = InMemoryThreadStore::new();
        for i in 0..5 {
            let t = make_thread(
                &format!("t{i}"),
                SystemTime::now() + Duration::from_secs(i * 10),
            );
            store.save_thread(&t).unwrap();
        }

        let page1 = store.list_threads(2, 0).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].id, "t4");

        let page2 = store.list_threads(2, 2).unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = store.list_threads(2, 4).unwrap();
        assert_eq!(page3.len(), 1);
    }

    #[test]
    fn archive_thread() {
        let store = InMemoryThreadStore::new();
        store
            .save_thread(&make_thread("t1", SystemTime::now()))
            .unwrap();
        store
            .save_thread(&make_thread("t2", SystemTime::now()))
            .unwrap();

        store.archive_thread("t1").unwrap();

        // Archived thread is excluded from listing.
        let listed = store.list_threads(10, 0).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "t2");

        // But can still be retrieved directly.
        let archived = store.get_thread("t1").unwrap().unwrap();
        assert!(archived.is_archived);

        // Archiving non-existent thread errors.
        assert!(store.archive_thread("nonexistent").is_err());
    }

    #[test]
    fn delete_thread_and_turns() {
        let store = InMemoryThreadStore::new();
        store
            .save_thread(&make_thread("t1", SystemTime::now()))
            .unwrap();
        store.save_turn(&make_turn("turn1", "t1", 0)).unwrap();
        store.save_turn(&make_turn("turn2", "t1", 1)).unwrap();

        store.delete_thread("t1").unwrap();

        assert!(store.get_thread("t1").unwrap().is_none());
        assert!(store.get_turns("t1", 10, 0).unwrap().is_empty());

        // Deleting non-existent thread errors.
        assert!(store.delete_thread("nonexistent").is_err());
    }

    #[test]
    fn save_and_retrieve_turns() {
        let store = InMemoryThreadStore::new();
        store
            .save_thread(&make_thread("t1", SystemTime::now()))
            .unwrap();
        store.save_turn(&make_turn("turn0", "t1", 0)).unwrap();
        store.save_turn(&make_turn("turn1", "t1", 1)).unwrap();
        store.save_turn(&make_turn("turn2", "t1", 2)).unwrap();

        let turns = store.get_turns("t1", 10, 0).unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].index, 0);
        assert_eq!(turns[1].index, 1);
        assert_eq!(turns[2].index, 2);

        // Pagination.
        let page = store.get_turns("t1", 2, 1).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].index, 1);
    }

    #[test]
    fn last_turn() {
        let store = InMemoryThreadStore::new();

        // No turns → None.
        assert!(store.last_turn("t1").unwrap().is_none());

        store.save_turn(&make_turn("turn0", "t1", 0)).unwrap();
        store.save_turn(&make_turn("turn2", "t1", 2)).unwrap();
        store.save_turn(&make_turn("turn1", "t1", 1)).unwrap();

        let last = store.last_turn("t1").unwrap().unwrap();
        assert_eq!(last.index, 2);
    }

    #[test]
    fn thread_stats_aggregation() {
        let store = InMemoryThreadStore::new();
        store.save_turn(&make_turn("turn0", "t1", 0)).unwrap();
        store.save_turn(&make_turn("turn1", "t1", 1)).unwrap();
        store.save_turn(&make_turn("turn2", "t1", 2)).unwrap();

        let stats = store.thread_stats("t1").unwrap();
        assert_eq!(stats.total_turns, 3);
        // input_tokens: 100 + 200 + 300 = 600
        assert_eq!(stats.total_input_tokens, 600);
        // output_tokens: 200 + 400 + 600 = 1200
        assert_eq!(stats.total_output_tokens, 1200);
        // Each turn has 1 tool call.
        assert_eq!(stats.total_tool_calls, 3);
        // duration: 1000 + 2000 + 3000 = 6000
        assert_eq!(stats.total_duration_ms, 6000);
        // Cost should be positive.
        assert!(stats.estimated_cost_usd > 0.0);

        // Empty thread stats.
        let empty_stats = store.thread_stats("nonexistent").unwrap();
        assert_eq!(empty_stats.total_turns, 0);
        assert!((empty_stats.estimated_cost_usd - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn save_thread_updates_existing() {
        let store = InMemoryThreadStore::new();
        let mut t = make_thread("t1", SystemTime::now());
        store.save_thread(&t).unwrap();

        t.title = Some("Updated Title".to_string());
        t.turn_count = 5;
        store.save_thread(&t).unwrap();

        let loaded = store.get_thread("t1").unwrap().unwrap();
        assert_eq!(loaded.title.as_deref(), Some("Updated Title"));
        assert_eq!(loaded.turn_count, 5);

        // Ensure no duplicates.
        let listed = store.list_threads(10, 0).unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn concurrent_access_safety() {
        use std::sync::Arc;

        let store = Arc::new(InMemoryThreadStore::new());
        let mut handles = Vec::new();

        // Spawn multiple threads saving concurrently.
        for i in 0..10 {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                let t = make_thread(
                    &format!("concurrent-{i}"),
                    SystemTime::now() + Duration::from_millis(i * 100),
                );
                store.save_thread(&t).unwrap();
                store
                    .save_turn(&make_turn(
                        &format!("turn-{i}"),
                        &format!("concurrent-{i}"),
                        0,
                    ))
                    .unwrap();
            }));
        }

        for handle in handles {
            handle.join().expect("thread panicked");
        }

        let threads = store.list_threads(20, 0).unwrap();
        assert_eq!(threads.len(), 10);
    }

    #[test]
    fn generate_ids_are_unique() {
        let ids: Vec<_> = (0..100).map(|_| generate_thread_id()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        // All generated IDs should be unique.
        assert_eq!(ids.len(), unique.len());

        let turn_ids: Vec<_> = (0..100).map(|_| generate_turn_id()).collect();
        let unique_turns: std::collections::HashSet<_> = turn_ids.iter().collect();
        assert_eq!(turn_ids.len(), unique_turns.len());
    }

    #[test]
    fn id_format() {
        let tid = generate_thread_id();
        assert!(tid.starts_with("thread_"), "unexpected format: {tid}");

        let uid = generate_turn_id();
        assert!(uid.starts_with("turn_"), "unexpected format: {uid}");
    }

    #[test]
    fn list_threads_excludes_archived() {
        let store = InMemoryThreadStore::new();
        let mut archived = make_thread("archived", SystemTime::now());
        archived.is_archived = true;
        store.save_thread(&archived).unwrap();
        store
            .save_thread(&make_thread("active", SystemTime::now()))
            .unwrap();

        let listed = store.list_threads(10, 0).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "active");
    }
}
