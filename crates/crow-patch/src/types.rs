//! Core data types for the patch contract.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── Primitives ─────────────────────────────────────────────────────

/// A workspace-relative path. Never an absolute OS path.
/// Guarantees: no leading `/`, no `..` traversal, UTF-8 clean.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct WorkspacePath(String);

// Custom deserializer that validates through new()
impl<'de> Deserialize<'de> for WorkspacePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        WorkspacePath::new(s).map_err(serde::de::Error::custom)
    }
}

impl WorkspacePath {
    /// Create a new workspace path, rejecting absolute or traversal paths.
    pub fn new(raw: impl Into<String>) -> Result<Self, PathError> {
        let s: String = raw.into();
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(PathError::Empty);
        }

        let path = std::path::Path::new(trimmed);
        for comp in path.components() {
            match comp {
                std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                    return Err(PathError::Absolute);
                }
                std::path::Component::ParentDir => {
                    return Err(PathError::Traversal);
                }
                std::path::Component::CurDir | std::path::Component::Normal(_) => {
                    // Safe components
                }
            }
        }

        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Convert to a full OS path given a workspace root.
    pub fn to_absolute(&self, root: &std::path::Path) -> PathBuf {
        root.join(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Empty,
    Absolute,
    Traversal,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::Empty => write!(f, "workspace path cannot be empty"),
            PathError::Absolute => write!(f, "workspace path must be relative"),
            PathError::Traversal => write!(f, "workspace path must not contain '..'"),
        }
    }
}

impl std::error::Error for PathError {}

// ─── Snapshot & Confidence ──────────────────────────────────────────

/// Opaque identifier for a workspace snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotId(pub String);

/// Confidence level attached to an intent.
///
/// Variants are ordered `None < Low < Medium < High` so that derived
/// `PartialOrd`/`Ord` produce semantically correct comparisons.
/// This is the **canonical definition** — `crow-evidence` re-exports it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Confidence {
    None,
    Low,
    Medium,
    High,
}

// ─── Preconditions ──────────────────────────────────────────────────

/// State the file *must* be in before a Modify patch can apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PreconditionState {
    /// SHA-256 hex digest of the file content at snapshot time.
    pub content_hash: String,
    /// Optional line-count anchor for sanity checking.
    pub expected_line_count: Option<usize>,
}

/// Lightweight precondition for non-Modify ops.
/// Every EditOp variant carries one of these so the apply layer can
/// reject drift before deleting or overwriting user work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum FilePrecondition {
    /// The path must NOT exist (used by Create to prevent silent overwrites).
    MustNotExist,
    /// The path must exist with this content hash (used by Delete, Rename source).
    MustExistWithHash(String),
    /// The path must exist (hash unchecked — weaker, for best-effort cases).
    MustExist,
}

// ─── Diff Hunks ─────────────────────────────────────────────────────

/// A single contiguous change region within a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiffHunk {
    /// 1-based start line in the original file.
    pub original_start: usize,
    /// Lines to remove, as a single multi-line string (empty = pure insertion).
    pub remove_block: String,
    /// Lines to insert, as a single multi-line string (empty = pure deletion).
    pub insert_block: String,
}

// ─── Reconnaissance Actions ─────────────────────────────────────────

/// A structured, schema-validated reconnaissance tool action.
/// Each variant declares exactly what inputs it accepts, eliminating
/// the argument-escape problem of the old generic `RunCommand`.
/// All filesystem paths are `WorkspacePath`, which rejects `..` and
/// absolute paths at the type level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "tool")]
pub enum ReconAction {
    /// List directory contents (like `ls -la`).
    #[serde(rename = "list_dir")]
    ListDir {
        /// Relative path to the directory. Use "." for workspace root.
        path: WorkspacePath,
    },
    /// Search for a pattern across files (like `rg` / `grep`).
    #[serde(rename = "search")]
    Search {
        /// The pattern to search for (literal string or regex).
        pattern: String,
        /// Optional: restrict search to a subdirectory.
        path: Option<WorkspacePath>,
        /// Optional: glob filter (e.g. "*.rs", "*.toml").
        glob: Option<String>,
    },
    /// Show file metadata (size, type, permissions).
    #[serde(rename = "file_info")]
    FileInfo { path: WorkspacePath },
    /// Count lines, words, and bytes in a file.
    #[serde(rename = "word_count")]
    WordCount { path: WorkspacePath },
    /// Show directory tree structure with a depth limit.
    #[serde(rename = "dir_tree")]
    DirTree {
        /// Relative path to the root of the tree.
        path: WorkspacePath,
        /// Maximum depth to display (default: 3, max: 10).
        max_depth: Option<u32>,
    },
}

// ─── Agent Action ───────────────────────────────────────────────────

/// An action the agent can take during the epistemic/crucible loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action")]
pub enum AgentAction {
    /// Read one or more files from the workspace.
    #[serde(rename = "read_files")]
    ReadFiles {
        paths: Vec<WorkspacePath>,
        rationale: String,
    },
    /// Execute a structured reconnaissance tool.
    #[serde(rename = "recon")]
    Recon {
        #[serde(flatten)]
        tool: ReconAction,
        rationale: String,
    },
    /// Submit a patch plan for hydration, application, and verification.
    #[serde(rename = "submit_plan")]
    SubmitPlan { plan: IntentPlan },
}

/// Validate semantic constraints that serde cannot enforce.
impl AgentAction {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            AgentAction::ReadFiles { paths, .. } => {
                if paths.is_empty() {
                    return Err("read_files requires at least one path".into());
                }
            }
            AgentAction::Recon { tool, .. } => match tool {
                ReconAction::Search { pattern, .. } => {
                    if pattern.trim().is_empty() {
                        return Err("search pattern must not be empty".into());
                    }
                }
                ReconAction::DirTree {
                    max_depth: Some(d), ..
                } => {
                    if *d > 10 {
                        return Err(format!("dir_tree max_depth {} exceeds limit of 10", d));
                    }
                }
                _ => {}
            },
            AgentAction::SubmitPlan { plan } => {
                if plan.operations.is_empty() {
                    return Err("submit_plan requires at least one operation".into());
                }
            }
        }
        Ok(())
    }
}

// ─── Edit Operations ────────────────────────────────────────────────

/// Strategy for handling conflicts on rename/create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ConflictStrategy {
    /// Fail if the target already exists.
    Fail,
    /// Overwrite the target (requires explicit user intent).
    Overwrite,
}

/// A single atomic filesystem mutation.
/// Every variant carries preconditions so the apply layer can reject
/// drift before touching user files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum EditOp {
    Modify {
        path: WorkspacePath,
        preconditions: PreconditionState,
        hunks: Vec<DiffHunk>,
    },
    Create {
        path: WorkspacePath,
        content: String,
        /// Must be `MustNotExist` unless `on_conflict: Overwrite`.
        precondition: FilePrecondition,
    },
    Rename {
        from: WorkspacePath,
        to: WorkspacePath,
        on_conflict: ConflictStrategy,
        /// Asserts the source file matches the snapshot.
        source_precondition: FilePrecondition,
        /// Asserts the destination state (typically `MustNotExist`).
        dest_precondition: FilePrecondition,
    },
    Delete {
        path: WorkspacePath,
        /// Asserts the file matches the snapshot before deleting.
        precondition: FilePrecondition,
    },
}

// ─── Intent Plan ────────────────────────────────────────────────────

/// The top-level container: a complete set of intended changes anchored
/// to a specific workspace snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntentPlan {
    /// The snapshot this plan was derived from. The materializer MUST
    /// reject application if the current workspace state diverges.
    pub base_snapshot_id: SnapshotId,
    /// Human-readable explanation of *why* these changes are proposed.
    pub rationale: String,
    /// If true, the model is expressing uncertainty and asking to probe
    /// further before committing.
    pub is_partial: bool,
    /// Model's self-assessed confidence.
    pub confidence: Confidence,
    /// The ordered list of atomic operations.
    pub operations: Vec<EditOp>,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // --- WorkspacePath validation ---

    #[test]
    fn valid_relative_path() {
        let p = WorkspacePath::new("src/main.rs").unwrap();
        assert_eq!(p.as_str(), "src/main.rs");
    }

    #[test]
    fn rejects_absolute_path() {
        assert_eq!(WorkspacePath::new("/etc/passwd"), Err(PathError::Absolute));
    }

    #[test]
    fn rejects_traversal() {
        assert_eq!(
            WorkspacePath::new("src/../../etc/passwd"),
            Err(PathError::Traversal)
        );
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(WorkspacePath::new(""), Err(PathError::Empty));
    }

    #[test]
    fn to_absolute_joins_correctly() {
        let root = PathBuf::from("/home/user/project");
        let p = WorkspacePath::new("src/lib.rs").unwrap();
        assert_eq!(
            p.to_absolute(&root),
            PathBuf::from("/home/user/project/src/lib.rs")
        );
    }

    // --- DiffHunk construction ---

    #[test]
    fn pure_insertion_hunk() {
        let h = DiffHunk {
            original_start: 10,
            remove_block: "".into(),
            insert_block: "// new comment\n".into(),
        };
        assert!(h.remove_block.is_empty());
        assert!(!h.insert_block.is_empty());
    }

    #[test]
    fn pure_deletion_hunk() {
        let h = DiffHunk {
            original_start: 5,
            remove_block: "old line\n".into(),
            insert_block: "".into(),
        };
        assert!(h.insert_block.is_empty());
    }

    // --- IntentPlan construction ---

    #[test]
    fn minimal_intent_plan() {
        let plan = IntentPlan {
            base_snapshot_id: SnapshotId("snap-001".into()),
            rationale: "Fix typo in README".into(),
            is_partial: false,
            confidence: Confidence::High,
            operations: vec![EditOp::Modify {
                path: WorkspacePath::new("README.md").unwrap(),
                preconditions: PreconditionState {
                    content_hash: "abc123".into(),
                    expected_line_count: Some(42),
                },
                hunks: vec![DiffHunk {
                    original_start: 3,
                    remove_block: "teh\n".into(),
                    insert_block: "the\n".into(),
                }],
            }],
        };
        assert_eq!(plan.operations.len(), 1);
        assert!(!plan.is_partial);
    }

    #[test]
    fn partial_exploratory_plan() {
        let plan = IntentPlan {
            base_snapshot_id: SnapshotId("snap-002".into()),
            rationale: "Not sure about auth refactor".into(),
            is_partial: true,
            confidence: Confidence::Low,
            operations: vec![],
        };
        assert!(plan.is_partial);
        assert_eq!(plan.confidence, Confidence::Low);
        assert!(plan.operations.is_empty());
    }

    #[test]
    fn multi_op_plan() {
        let plan = IntentPlan {
            base_snapshot_id: SnapshotId("snap-003".into()),
            rationale: "Rename module and update imports".into(),
            is_partial: false,
            confidence: Confidence::Medium,
            operations: vec![
                EditOp::Rename {
                    from: WorkspacePath::new("src/old.rs").unwrap(),
                    to: WorkspacePath::new("src/new.rs").unwrap(),
                    on_conflict: ConflictStrategy::Fail,
                    source_precondition: FilePrecondition::MustExistWithHash("aaa".into()),
                    dest_precondition: FilePrecondition::MustNotExist,
                },
                EditOp::Modify {
                    path: WorkspacePath::new("src/lib.rs").unwrap(),
                    preconditions: PreconditionState {
                        content_hash: "def456".into(),
                        expected_line_count: None,
                    },
                    hunks: vec![DiffHunk {
                        original_start: 1,
                        remove_block: "mod old;\n".into(),
                        insert_block: "mod new;\n".into(),
                    }],
                },
            ],
        };
        assert_eq!(plan.operations.len(), 2);
    }

    // --- FilePrecondition coverage ---

    #[test]
    fn create_with_must_not_exist() {
        let op = EditOp::Create {
            path: WorkspacePath::new("new_file.rs").unwrap(),
            content: "fn main() {}".into(),
            precondition: FilePrecondition::MustNotExist,
        };
        match &op {
            EditOp::Create { precondition, .. } => {
                assert_eq!(*precondition, FilePrecondition::MustNotExist);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn delete_with_hash_precondition() {
        let op = EditOp::Delete {
            path: WorkspacePath::new("obsolete.rs").unwrap(),
            precondition: FilePrecondition::MustExistWithHash("deadbeef".into()),
        };
        match &op {
            EditOp::Delete { precondition, .. } => {
                assert_eq!(
                    *precondition,
                    FilePrecondition::MustExistWithHash("deadbeef".into())
                );
            }
            _ => panic!("wrong variant"),
        }
    }

    // --- ReconAction & validation coverage ---

    #[test]
    fn recon_list_dir_roundtrip() {
        let json = r#"{"action":"recon","tool":"list_dir","path":"src","rationale":"list"}"#;
        let action: AgentAction = serde_json::from_str(json).unwrap();
        match &action {
            AgentAction::Recon {
                tool: ReconAction::ListDir { path },
                ..
            } => assert_eq!(path.as_str(), "src"),
            _ => panic!("wrong variant: {:?}", action),
        }
        assert!(action.validate().is_ok());
    }

    #[test]
    fn recon_search_roundtrip() {
        let json = r#"{"action":"recon","tool":"search","pattern":"fn main","path":"src","glob":"*.rs","rationale":"find mains"}"#;
        let action: AgentAction = serde_json::from_str(json).unwrap();
        match &action {
            AgentAction::Recon {
                tool:
                    ReconAction::Search {
                        pattern,
                        path,
                        glob,
                    },
                ..
            } => {
                assert_eq!(pattern, "fn main");
                assert_eq!(path.as_ref().unwrap().as_str(), "src");
                assert_eq!(glob.as_deref(), Some("*.rs"));
            }
            _ => panic!("wrong variant"),
        }
        assert!(action.validate().is_ok());
    }

    #[test]
    fn recon_search_empty_pattern_rejected() {
        let action = AgentAction::Recon {
            tool: ReconAction::Search {
                pattern: "  ".into(),
                path: None,
                glob: None,
            },
            rationale: "test".into(),
        };
        assert!(action.validate().is_err());
    }

    #[test]
    fn recon_dir_tree_roundtrip() {
        let json =
            r#"{"action":"recon","tool":"dir_tree","path":".","max_depth":5,"rationale":"tree"}"#;
        let action: AgentAction = serde_json::from_str(json).unwrap();
        match &action {
            AgentAction::Recon {
                tool: ReconAction::DirTree { max_depth, .. },
                ..
            } => assert_eq!(*max_depth, Some(5)),
            _ => panic!("wrong variant"),
        }
        assert!(action.validate().is_ok());
    }

    #[test]
    fn recon_dir_tree_excessive_depth_rejected() {
        let action = AgentAction::Recon {
            tool: ReconAction::DirTree {
                path: WorkspacePath::new(".").unwrap(),
                max_depth: Some(99),
            },
            rationale: "test".into(),
        };
        assert!(action.validate().is_err());
    }

    #[test]
    fn validate_empty_read_files_rejected() {
        let action = AgentAction::ReadFiles {
            paths: vec![],
            rationale: "test".into(),
        };
        assert!(action.validate().is_err());
    }

    #[test]
    fn validate_empty_submit_plan_rejected() {
        let action = AgentAction::SubmitPlan {
            plan: IntentPlan {
                base_snapshot_id: SnapshotId("s".into()),
                rationale: "empty".into(),
                is_partial: false,
                confidence: Confidence::Low,
                operations: vec![],
            },
        };
        assert!(action.validate().is_err());
    }
}
