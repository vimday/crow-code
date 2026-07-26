//! AGENTS.md hierarchical discovery and instruction assembly.
//!
//! Implements Codex-style project documentation discovery by walking
//! from the current working directory up to the project root, collecting
//! all instruction files found along the way.
//!
//! # Discovery Algorithm
//!
//! 1. Determine the project root by walking upward from `cwd` until a
//!    root marker (`.git`, `Cargo.toml`, `package.json`, etc.) is found.
//! 2. Collect every instruction file from the project root down to `cwd`
//!    (inclusive), concatenating their contents in root-to-leaf order.
//! 3. **Deduplicate** by content hash (claw-code pattern) — symlinked or
//!    copied files are only included once.
//! 4. **Truncate** individual files to a per-file budget and the total
//!    assembly to a global budget, to prevent context bloat.
//! 5. Do **not** walk past the project root.
//!
//! This allows nested directories to provide progressively more specific
//! instructions that augment (not replace) the project-level rules.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Default filename for agent instructions.
pub const DEFAULT_AGENTS_MD_FILENAME: &str = "AGENTS.md";

/// Filenames to search for in each directory (checked in order).
/// A directory uses the first matching file found.
pub const AGENTS_MD_FILENAMES: &[&str] = &["AGENTS.md", "AGENTS.override.md", ".agents.md"];

/// Additional filenames searched in `.crow/` subdirectories.
/// These support the claw-code pattern of placing instructions in a
/// hidden config directory.
pub const DOTCROW_FILENAMES: &[&str] = &["AGENTS.md", "instructions.md", "CROW.md"];

/// Separator between AGENTS.md sections from different directories.
const AGENTS_MD_SEPARATOR: &str = "\n\n--- project-doc ---\n\n";

/// Maximum bytes for a single instruction file before truncation.
const MAX_FILE_BYTES: usize = 4 * 1024; // 4 KB

/// Maximum total bytes for all instruction files combined.
const MAX_TOTAL_BYTES: usize = 12 * 1024; // 12 KB

/// Default root markers that identify a project root directory.
const ROOT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "Makefile",
    ".crow",
];

/// A single discovered context file with its source path and content.
#[derive(Debug, Clone)]
pub struct ContextFile {
    /// Absolute path of the discovered file.
    pub path: PathBuf,
    /// Content of the file (possibly truncated).
    pub content: String,
    /// Whether the content was truncated to fit the per-file budget.
    pub truncated: bool,
}

/// Result of AGENTS.md discovery.
#[derive(Debug, Clone)]
pub struct AgentsMdResult {
    /// Concatenated AGENTS.md content from all discovered files.
    pub content: String,
    /// Paths of all discovered AGENTS.md files (root-to-leaf order).
    pub sources: Vec<PathBuf>,
    /// Individual context files with metadata.
    pub files: Vec<ContextFile>,
}

/// Find the project root by walking up from `start` until a root marker is found.
///
/// Returns `None` if no marker is found (in which case only `start` itself
/// should be searched for AGENTS.md).
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        for marker in ROOT_MARKERS {
            if dir.join(marker).exists() {
                return Some(dir);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Compute a simple content hash for deduplication.
///
/// Uses FNV-1a (64-bit) — the same algorithm as claw-code's
/// `workspace_fingerprint` — for fast, deterministic hashing
/// without external dependencies.
fn content_hash(content: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Discover and load instruction files hierarchically.
///
/// Walks from the project root down to `cwd`, collecting all instruction
/// files found along the path. Returns the concatenated content with
/// separator markers, or `None` if no files are found.
///
/// **Deduplication**: Files with identical content (by FNV-1a hash) are
/// included only once, preventing symlinks or copies from inflating context.
///
/// **Truncation**: Individual files are capped at 4KB, and the total
/// assembly is capped at 12KB, to prevent context window bloat.
pub fn discover_agents_md(cwd: &Path) -> Option<AgentsMdResult> {
    let project_root = find_project_root(cwd).unwrap_or_else(|| cwd.to_path_buf());

    // Build the path chain from project root to cwd
    let mut path_chain = Vec::new();
    let mut current = cwd.to_path_buf();

    // Collect directories from cwd up to (and including) project_root
    loop {
        path_chain.push(current.clone());
        if current == project_root || !current.pop() {
            break;
        }
    }
    // Reverse to get root-to-leaf order
    path_chain.reverse();

    let mut seen_hashes: HashSet<u64> = HashSet::new();
    let mut context_files = Vec::new();
    let mut sources = Vec::new();
    let mut sections = Vec::new();
    let mut total_bytes = 0usize;

    for dir in &path_chain {
        // Check standard filenames in the directory itself
        if let Some(cf) =
            try_load_instruction_file(dir, AGENTS_MD_FILENAMES, &project_root, &mut seen_hashes)
        {
            total_bytes += cf.content.len();
            sources.push(cf.path.clone());
            let relative = cf.path.strip_prefix(&project_root).unwrap_or(&cf.path);
            sections.push(format!("# From: {}\n\n{}", relative.display(), cf.content));
            context_files.push(cf);
        }

        // Check .crow/ subdirectory for additional instruction files
        let dotcrow = dir.join(".crow");
        if dotcrow.is_dir() {
            if let Some(cf) = try_load_instruction_file(
                &dotcrow,
                DOTCROW_FILENAMES,
                &project_root,
                &mut seen_hashes,
            ) {
                total_bytes += cf.content.len();
                sources.push(cf.path.clone());
                let relative = cf.path.strip_prefix(&project_root).unwrap_or(&cf.path);
                sections.push(format!("# From: {}\n\n{}", relative.display(), cf.content));
                context_files.push(cf);
            }
        }

        // Bail early if we've hit the global budget
        if total_bytes >= MAX_TOTAL_BYTES {
            break;
        }
    }

    if sections.is_empty() {
        return None;
    }

    // Enforce global budget by truncating the concatenated output
    let mut content = sections.join(AGENTS_MD_SEPARATOR);
    if content.len() > MAX_TOTAL_BYTES {
        content = crow_patch::safe_truncate(&content, MAX_TOTAL_BYTES).to_string();
        content.push_str("\n\n[SYSTEM: Instruction files truncated to 12KB budget]");
    }

    Some(AgentsMdResult {
        content,
        sources,
        files: context_files,
    })
}

/// Try to load the first matching instruction file from a directory.
///
/// Returns `None` if no matching file is found, or if the file's content
/// hash has already been seen (dedup).
fn try_load_instruction_file(
    dir: &Path,
    filenames: &[&str],
    _project_root: &Path,
    seen_hashes: &mut HashSet<u64>,
) -> Option<ContextFile> {
    for filename in filenames {
        let candidate = dir.join(filename);
        if let Ok(raw_content) = std::fs::read_to_string(&candidate) {
            let trimmed = raw_content.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Content-hash dedup (claw-code pattern)
            let hash = content_hash(trimmed);
            if !seen_hashes.insert(hash) {
                // Already seen this exact content — skip
                continue;
            }

            // Per-file truncation
            let (content, truncated) = if trimmed.len() > MAX_FILE_BYTES {
                let truncated_str = crow_patch::safe_truncate(trimmed, MAX_FILE_BYTES);
                (
                    format!("{truncated_str}\n\n[SYSTEM: File truncated to 4KB budget]"),
                    true,
                )
            } else {
                (trimmed.to_string(), false)
            };

            return Some(ContextFile {
                path: candidate,
                content,
                truncated,
            });
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn find_project_root_finds_git() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        fs::create_dir(root.join(".git")).expect("create .git");
        let sub = root.join("src");
        fs::create_dir_all(&sub).expect("create src");

        let found = find_project_root(&sub);
        assert_eq!(found, Some(root.to_path_buf()));
    }

    #[test]
    fn find_project_root_finds_cargo_toml() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("Cargo.toml"), "[package]").expect("write");
        let sub = root.join("crates").join("core");
        fs::create_dir_all(&sub).expect("create dirs");

        let found = find_project_root(&sub);
        assert_eq!(found, Some(root.to_path_buf()));
    }

    #[test]
    fn discover_agents_md_finds_hierarchical() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        fs::create_dir(root.join(".git")).expect("create .git");

        // Root-level AGENTS.md
        fs::write(root.join("AGENTS.md"), "# Project Rules\nBe excellent.")
            .expect("write root agents");

        // Subdirectory AGENTS.md
        let sub = root.join("crates").join("core");
        fs::create_dir_all(&sub).expect("create dirs");
        fs::write(sub.join("AGENTS.md"), "# Core Rules\nUse Rust idioms.")
            .expect("write sub agents");

        let result = discover_agents_md(&sub).expect("should discover");
        assert_eq!(result.sources.len(), 2);
        assert_eq!(result.files.len(), 2);
        assert!(result.content.contains("Project Rules"));
        assert!(result.content.contains("Core Rules"));
        assert!(result.content.contains("project-doc"));
    }

    #[test]
    fn discover_agents_md_deduplicates_by_content() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        fs::create_dir(root.join(".git")).expect("create .git");

        let content = "# Shared Rules\nDon't repeat yourself.";
        fs::write(root.join("AGENTS.md"), content).expect("write root");

        let sub = root.join("src");
        fs::create_dir_all(&sub).expect("create src");
        // Same content in subdirectory — should be deduped
        fs::write(sub.join("AGENTS.md"), content).expect("write sub");

        let result = discover_agents_md(&sub).expect("should discover");
        assert_eq!(
            result.files.len(),
            1,
            "identical content should be deduplicated"
        );
    }

    #[test]
    fn discover_agents_md_finds_dotcrow_instructions() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        fs::create_dir(root.join(".git")).expect("create .git");

        // .crow/instructions.md
        let dotcrow = root.join(".crow");
        fs::create_dir_all(&dotcrow).expect("create .crow");
        fs::write(
            dotcrow.join("instructions.md"),
            "# Custom Rules\nFollow these.",
        )
        .expect("write instructions");

        let result = discover_agents_md(root).expect("should discover");
        assert_eq!(result.files.len(), 1);
        assert!(result.content.contains("Custom Rules"));
    }

    #[test]
    fn discover_agents_md_truncates_large_files() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        fs::create_dir(root.join(".git")).expect("create .git");

        let large = "x".repeat(10_000);
        fs::write(root.join("AGENTS.md"), &large).expect("write large");

        let result = discover_agents_md(root).expect("should discover");
        assert!(result.files[0].truncated);
        assert!(result.files[0].content.len() < large.len());
        assert!(result.files[0].content.contains("[SYSTEM: File truncated"));
    }

    #[test]
    fn discover_agents_md_none_when_empty() {
        let tmp = TempDir::new().expect("tempdir");
        let result = discover_agents_md(tmp.path());
        assert!(result.is_none());
    }

    #[test]
    fn content_hash_is_deterministic() {
        let h1 = content_hash("hello world");
        let h2 = content_hash("hello world");
        let h3 = content_hash("hello world!");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }
}
