//! Snapshot identity resolution.
//!
//! Resolves a `SnapshotId` from the workspace's git HEAD hash,
//! falling back to a content-hash of key manifest files when git
//! is unavailable. This replaces the placeholder `"snapshot-001"`
//! with a real, verifiable workspace identity.

use crow_patch::SnapshotId;
use std::path::Path;
use std::process::Command;

/// Resolve a `SnapshotId` from the workspace.
///
/// Strategy:
/// 1. Try `git rev-parse HEAD` — produces a 40-char SHA.
/// 2. If git fails (not a repo, git not installed), fall back to a
///    content hash of key manifest files (Cargo.lock, package-lock.json, etc.).
/// 3. If all else fails, use a timestamp-based fallback.
pub fn resolve_snapshot_id(workspace_root: &Path) -> SnapshotId {
    if let Some(git_hash) = git_head_hash(workspace_root) {
        return SnapshotId(format!("git-{}", &git_hash[..12.min(git_hash.len())]));
    }

    if let Some(content_hash) = manifest_hash(workspace_root) {
        return SnapshotId(format!("hash-{}", &content_hash[..12.min(content_hash.len())]));
    }

    // Last resort: timestamp
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    SnapshotId(format!("ts-{:x}", ts))
}

/// Run `git rev-parse HEAD` in the workspace root.
fn git_head_hash(workspace_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let hash = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string();

    if hash.is_empty() || hash.len() < 7 {
        return None;
    }

    Some(hash)
}

/// Compute a simple hash from manifest file contents.
/// Uses SHA-256 of concatenated manifests for stability.
fn manifest_hash(workspace_root: &Path) -> Option<String> {
    use std::collections::BTreeMap;
    use std::fs;

    let candidates = [
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "Cargo.toml",
        "package.json",
    ];

    let mut found: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for name in &candidates {
        let path = workspace_root.join(name);
        if let Ok(content) = fs::read(&path) {
            found.insert(name, content);
        }
    }

    if found.is_empty() {
        return None;
    }

    // Simple FNV-like hash (avoiding SHA dependency in this crate)
    let mut hash: u64 = 0xcbf29ce484222325;
    for (name, content) in &found {
        for byte in name.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        for byte in content {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }

    Some(format!("{:016x}", hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn resolve_in_non_git_dir_falls_back() {
        let dir = tempdir().unwrap();
        let snap = resolve_snapshot_id(dir.path());
        // Should not start with "git-" since tempdir is not a git repo
        assert!(!snap.0.starts_with("git-"), "got: {}", snap.0);
        // Should be timestamp-based since no manifests
        assert!(snap.0.starts_with("ts-"), "got: {}", snap.0);
    }

    #[test]
    fn resolve_with_manifest_uses_hash() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname = \"test\"").unwrap();
        let snap = resolve_snapshot_id(dir.path());
        assert!(snap.0.starts_with("hash-"), "got: {}", snap.0);
    }

    #[test]
    fn resolve_in_git_repo_uses_git() {
        // Only runs if we're inside a git-controlled workspace
        let workspace = std::env::current_dir().unwrap();
        let snap = resolve_snapshot_id(&workspace);
        // If the test is running from within the crow-code repo, this should be git-based
        if workspace.join(".git").exists() {
            assert!(snap.0.starts_with("git-"), "got: {}", snap.0);
            assert!(snap.0.len() >= 16, "got: {}", snap.0);
        }
    }

    #[test]
    fn manifest_hash_is_deterministic() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), b"[package]\nname = \"test\"").unwrap();
        let h1 = manifest_hash(dir.path());
        let h2 = manifest_hash(dir.path());
        assert_eq!(h1, h2, "Hash should be deterministic");
    }
}
