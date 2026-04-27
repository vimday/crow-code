//! AGENTS.md hierarchical discovery and instruction assembly.
//!
//! Implements Codex-style project documentation discovery by walking
//! from the current working directory up to the project root, collecting
//! all `AGENTS.md` files found along the way.
//!
//! # Discovery Algorithm
//!
//! 1. Determine the project root by walking upward from `cwd` until a
//!    root marker (`.git`, `Cargo.toml`, `package.json`, etc.) is found.
//! 2. Collect every `AGENTS.md` from the project root down to `cwd`
//!    (inclusive), concatenating their contents in root-to-leaf order.
//! 3. Do **not** walk past the project root.
//!
//! This allows nested directories to provide progressively more specific
//! instructions that augment (not replace) the project-level rules.

use std::path::{Path, PathBuf};

/// Default filename for agent instructions.
pub const DEFAULT_AGENTS_MD_FILENAME: &str = "AGENTS.md";

/// Filenames to search for in each directory (checked in order).
pub const AGENTS_MD_FILENAMES: &[&str] = &["AGENTS.md", "AGENTS.override.md", ".agents.md"];

/// Separator between AGENTS.md sections from different directories.
const AGENTS_MD_SEPARATOR: &str = "\n\n--- project-doc ---\n\n";

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

/// Result of AGENTS.md discovery.
#[derive(Debug, Clone)]
pub struct AgentsMdResult {
    /// Concatenated AGENTS.md content from all discovered files.
    pub content: String,
    /// Paths of all discovered AGENTS.md files (root-to-leaf order).
    pub sources: Vec<PathBuf>,
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

/// Discover and load AGENTS.md files hierarchically.
///
/// Walks from the project root down to `cwd`, collecting all AGENTS.md
/// files found along the path. Returns the concatenated content with
/// separator markers, or `None` if no files are found.
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

    let mut sources = Vec::new();
    let mut sections = Vec::new();

    for dir in &path_chain {
        for filename in AGENTS_MD_FILENAMES {
            let candidate = dir.join(filename);
            if let Ok(content) = std::fs::read_to_string(&candidate) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    // Add a header indicating which file this section came from
                    let relative = candidate.strip_prefix(&project_root).unwrap_or(&candidate);
                    sections.push(format!("# From: {}\n\n{trimmed}", relative.display()));
                    sources.push(candidate);
                    // Only use the first matching filename per directory
                    break;
                }
            }
        }
    }

    if sections.is_empty() {
        return None;
    }

    Some(AgentsMdResult {
        content: sections.join(AGENTS_MD_SEPARATOR),
        sources,
    })
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
        assert!(result.content.contains("Project Rules"));
        assert!(result.content.contains("Core Rules"));
        assert!(result.content.contains("project-doc"));
    }

    #[test]
    fn discover_agents_md_none_when_empty() {
        let tmp = TempDir::new().expect("tempdir");
        let result = discover_agents_md(tmp.path());
        assert!(result.is_none());
    }
}
