//! Session persistence for crow.
//!
//! Stores conversation history and snapshot timelines.
//! Sessions are serialized as JSON files under `~/.crow/sessions/`.

use anyhow::{Context, Result};
use crow_brain::{ChatMessage, ChatRole};
use crow_patch::SnapshotId;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

// ─── Types ──────────────────────────────────────────────────────────

/// Unique session identifier (UUID-based).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionId(pub String);

/// A serializable chat message suitable for JSON persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMessage {
    role: String,
    content: String,
}

impl From<&ChatMessage> for StoredMessage {
    fn from(msg: &ChatMessage) -> Self {
        let role = match msg.role {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        };
        Self {
            role: role.to_string(),
            content: msg.content.clone(),
        }
    }
}

impl StoredMessage {
    fn to_chat_message(&self) -> ChatMessage {
        match self.role.as_str() {
            "system" => ChatMessage::system(&self.content),
            "user" => ChatMessage::user(&self.content),
            "assistant" => ChatMessage::assistant(&self.content),
            _ => ChatMessage::user(&self.content), // fallback
        }
    }
}

/// A persisted crow session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier.
    pub id: SessionId,
    /// Workspace root this session is associated with.
    pub workspace_root: PathBuf,
    /// Task/prompt that initiated this session.
    pub task: String,
    /// Stored conversation messages.
    messages: Vec<StoredMessage>,
    /// Snapshot timeline — each SnapshotId represents a verified state.
    pub snapshot_timeline: Vec<SnapshotId>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 last-updated timestamp.
    pub updated_at: String,
}

impl Session {
    /// Create a new session for the given workspace and task.
    pub fn new(workspace: &Path, task: &str) -> Self {
        let now = chrono_now();
        Self {
            id: SessionId(generate_id()),
            workspace_root: workspace.to_path_buf(),
            task: task.to_string(),
            messages: Vec::new(),
            snapshot_timeline: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Store the current conversation state.
    pub fn save_messages(&mut self, messages: &[ChatMessage]) {
        self.messages = messages.iter().map(StoredMessage::from).collect();
        self.updated_at = chrono_now();
    }

    /// Restore the conversation as `ChatMessage`s.
    pub fn restore_messages(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .map(StoredMessage::to_chat_message)
            .collect()
    }

    /// Record a new snapshot in the timeline.
    pub fn push_snapshot(&mut self, snapshot: SnapshotId) {
        self.snapshot_timeline.push(snapshot);
        self.updated_at = chrono_now();
    }
}

// ─── Session Store ──────────────────────────────────────────────────

/// Manages session persistence on disk.
///
/// Sessions are stored as individual JSON files under `~/.crow/sessions/`.
/// The store maintains session state in the user's home directory rather
/// than polluting the local workspace repository.
pub struct SessionStore {
    session_dir: PathBuf,
}

impl SessionStore {
    /// Create a new session store. Creates the directory if needed.
    pub fn open() -> Result<Self> {
        let home = dirs_home()?;
        let session_dir = home.join(".crow").join("sessions");
        fs::create_dir_all(&session_dir).with_context(|| {
            format!(
                "Failed to create session directory: {}",
                session_dir.display()
            )
        })?;
        Ok(Self { session_dir })
    }

    /// Save a session to disk.
    pub fn save(&self, session: &Session) -> Result<()> {
        let path = self.session_path(&session.id);
        let json = serde_json::to_string_pretty(session).context("Failed to serialize session")?;
        fs::write(&path, json)
            .with_context(|| format!("Failed to write session to {}", path.display()))?;
        Ok(())
    }

    /// Load a session by ID.
    pub fn load(&self, id: &SessionId) -> Result<Session> {
        let path = self.session_path(id);
        let json = fs::read_to_string(&path)
            .with_context(|| format!("Session not found: {}", path.display()))?;
        let session: Session =
            serde_json::from_str(&json).context("Failed to parse session JSON")?;
        Ok(session)
    }

    /// List all saved sessions, newest first.
    pub fn list(&self) -> Result<Vec<SessionSummary>> {
        let mut summaries = Vec::new();

        for entry in fs::read_dir(&self.session_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            if let Ok(json) = fs::read_to_string(&path) {
                if let Ok(session) = serde_json::from_str::<Session>(&json) {
                    summaries.push(SessionSummary {
                        id: session.id,
                        workspace: session.workspace_root,
                        task: session.task,
                        snapshots: session.snapshot_timeline.len(),
                        updated_at: session.updated_at,
                    });
                }
            }
        }

        // Sort newest first
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }

    /// Find the most recent session for a workspace.
    #[allow(dead_code)]
    pub fn find_latest_for_workspace(&self, workspace: &Path) -> Result<Option<Session>> {
        let summaries = self.list()?;
        for summary in summaries {
            if summary.workspace == workspace {
                return self.load(&summary.id).map(Some);
            }
        }
        Ok(None)
    }

    fn session_path(&self, id: &SessionId) -> PathBuf {
        self.session_dir.join(format!("{}.json", id.0))
    }
}

/// Lightweight session metadata for listing.
#[derive(Debug)]
pub struct SessionSummary {
    pub id: SessionId,
    #[allow(dead_code)]
    pub workspace: PathBuf,
    pub task: String,
    pub snapshots: usize,
    pub updated_at: String,
}

impl std::fmt::Display for SessionSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "  {} │ {} │ {} snapshots │ {}",
            &self.id.0[..8.min(self.id.0.len())],
            truncate_str(&self.task, 40),
            self.snapshots,
            self.updated_at,
        )
    }
}

// ─── Utilities ──────────────────────────────────────────────────────

fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{ts:x}")
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // ISO 8601 approximate (no chrono dependency)
    format!("{secs}")
}

fn dirs_home() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .context("Could not determine home directory (set HOME or USERPROFILE)")
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_roundtrip() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("my_project");
        fs::create_dir_all(&ws).unwrap();

        let mut session = Session::new(&ws, "fix the auth bug");

        // Add some messages
        session.save_messages(&[
            ChatMessage::system("You are crow."),
            ChatMessage::user("Fix the login function"),
            ChatMessage::assistant("I'll modify auth.rs"),
        ]);

        // Add snapshot
        session.push_snapshot(SnapshotId("snap_abc123".into()));

        // Serialize and deserialize
        let json = serde_json::to_string_pretty(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.id, session.id);
        assert_eq!(restored.workspace_root, ws);
        assert_eq!(restored.task, "fix the auth bug");
        assert_eq!(restored.snapshot_timeline.len(), 1);

        let msgs = restored.restore_messages();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "You are crow.");
        assert_eq!(msgs[2].content, "I'll modify auth.rs");
    }

    #[test]
    fn session_store_save_list_load() {
        let dir = tempdir().unwrap();
        // Override session dir for test isolation
        let store = SessionStore {
            session_dir: dir.path().to_path_buf(),
        };

        let ws = dir.path().join("project_a");
        fs::create_dir_all(&ws).unwrap();

        let session = Session::new(&ws, "add error handling");
        let id = session.id.clone();

        store.save(&session).unwrap();

        // List should find it
        let summaries = store.list().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].task, "add error handling");

        // Load should restore it
        let loaded = store.load(&id).unwrap();
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.task, "add error handling");
    }

    #[test]
    fn find_latest_for_workspace() {
        let dir = tempdir().unwrap();
        let store = SessionStore {
            session_dir: dir.path().to_path_buf(),
        };

        let ws_a = dir.path().join("project_a");
        let ws_b = dir.path().join("project_b");
        fs::create_dir_all(&ws_a).unwrap();
        fs::create_dir_all(&ws_b).unwrap();

        store.save(&Session::new(&ws_a, "task a")).unwrap();
        store.save(&Session::new(&ws_b, "task b")).unwrap();

        let found = store.find_latest_for_workspace(&ws_a).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().task, "task a");

        let not_found = store
            .find_latest_for_workspace(Path::new("/no/such/dir"))
            .unwrap();
        assert!(not_found.is_none());
    }
}
