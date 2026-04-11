//! Materialization driver implementations.
//!
//! Each driver implements the same contract: given a source directory and
//! config, produce an isolated sandbox directory.
//!
//! # Invariants
//!
//! - The source workspace is **never modified**.
//! - Artifact directories are created as **empty directories** in the
//!   sandbox — they are never symlinked or copied from the source.
//! - Source symlinks are preserved (re-created in the sandbox).

use crate::types::{MaterializeConfig, MaterializationDriver, SandboxHandle};
use std::fs;
use std::path::{Path, PathBuf};

/// Execute a specific driver to create a sandbox.
pub fn execute(
    driver: &MaterializationDriver,
    config: &MaterializeConfig,
) -> Result<SandboxHandle, String> {
    match driver {
        MaterializationDriver::SafeCopy => safe_copy(config),
        MaterializationDriver::ApfsClone => apfs_clone(config),
        MaterializationDriver::HardlinkTree => hardlink_tree(config),
        MaterializationDriver::CowClone => cow_clone(config),
    }
}

// ─── Sandbox directory creation ─────────────────────────────────────

fn create_sandbox_dir() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let sandbox_path = std::env::temp_dir().join(format!("crow_vfs_{}_{}", id, seq));
    fs::create_dir_all(&sandbox_path)
        .map_err(|e| format!("failed to create sandbox dir: {}", e))?;
    Ok(sandbox_path)
}

// ─── Helpers ────────────────────────────────────────────────────────

/// Check if a directory name matches any artifact_dirs entry.
fn is_artifact_dir(name: &str, config: &MaterializeConfig) -> bool {
    config.artifact_dirs.iter().any(|a| a == name)
}

/// Check if a path component matches any skip pattern.
fn should_skip(name: &str, config: &MaterializeConfig) -> bool {
    config.skip_patterns.iter().any(|p| {
        let pattern = p.trim_end_matches('/');
        name == pattern
    })
}

/// Re-create a symlink in the sandbox, preserving its target.
#[cfg(unix)]
fn recreate_symlink(src_path: &Path, dst_path: &Path) -> Result<(), String> {
    let target = fs::read_link(src_path)
        .map_err(|e| format!("readlink {} failed: {}", src_path.display(), e))?;
    std::os::unix::fs::symlink(&target, dst_path)
        .map_err(|e| format!("symlink {} failed: {}", dst_path.display(), e))?;
    Ok(())
}

#[cfg(not(unix))]
fn recreate_symlink(_src_path: &Path, _dst_path: &Path) -> Result<(), String> {
    Err("symlink preservation not yet supported on this platform".into())
}

// ─── SafeCopy Driver ────────────────────────────────────────────────

fn safe_copy(config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    let sandbox = create_sandbox_dir()?;
    safe_copy_tree(&config.source, &sandbox, config)?;
    Ok(SandboxHandle::new(sandbox, MaterializationDriver::SafeCopy))
}

fn safe_copy_tree(
    source: &Path,
    dest: &Path,
    config: &MaterializeConfig,
) -> Result<(), String> {
    let entries = fs::read_dir(source)
        .map_err(|e| format!("failed to read {}: {}", source.display(), e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry error: {}", e))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let src_path = entry.path();
        let dst_path = dest.join(&*name);

        if should_skip(&name, config) {
            continue;
        }

        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type error: {}", e))?;

        if file_type.is_symlink() {
            // Preserve symlinks before checking is_dir/is_file
            // (which follow symlinks).
            recreate_symlink(&src_path, &dst_path)?;
        } else if file_type.is_dir() {
            if is_artifact_dir(&name, config) {
                // Create empty directory — never copy or symlink artifact dirs.
                fs::create_dir_all(&dst_path)
                    .map_err(|e| format!("mkdir {} failed: {}", dst_path.display(), e))?;
            } else {
                fs::create_dir_all(&dst_path)
                    .map_err(|e| format!("mkdir {} failed: {}", dst_path.display(), e))?;
                safe_copy_tree(&src_path, &dst_path, config)?;
            }
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("copy {} failed: {}", name, e))?;
        }
    }
    Ok(())
}

// ─── APFS Clone Driver (macOS) ──────────────────────────────────────

#[cfg(target_os = "macos")]
fn apfs_clone(config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    let sandbox = create_sandbox_dir()?;
    apfs_clone_tree(&config.source, &sandbox, config)?;
    Ok(SandboxHandle::new(sandbox, MaterializationDriver::ApfsClone))
}

#[cfg(target_os = "macos")]
fn apfs_clone_tree(
    source: &Path,
    dest: &Path,
    config: &MaterializeConfig,
) -> Result<(), String> {
    let entries = fs::read_dir(source)
        .map_err(|e| format!("read_dir: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry: {}", e))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let src_path = entry.path();
        let dst_path = dest.join(&*name);

        if should_skip(&name, config) {
            continue;
        }

        let file_type = entry.file_type().map_err(|e| format!("file_type: {}", e))?;

        if file_type.is_symlink() {
            recreate_symlink(&src_path, &dst_path)?;
        } else if file_type.is_dir() {
            if is_artifact_dir(&name, config) {
                fs::create_dir_all(&dst_path)
                    .map_err(|e| format!("mkdir: {}", e))?;
            } else {
                fs::create_dir_all(&dst_path)
                    .map_err(|e| format!("mkdir: {}", e))?;
                apfs_clone_tree(&src_path, &dst_path, config)?;
            }
        } else if file_type.is_file() {
            // Try clonefile first, fall back to copy
            let src_c = std::ffi::CString::new(src_path.to_string_lossy().as_bytes())
                .map_err(|_| "invalid path for clonefile")?;
            let dst_c = std::ffi::CString::new(dst_path.to_string_lossy().as_bytes())
                .map_err(|_| "invalid path for clonefile")?;
            let ret = unsafe {
                libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0)
            };
            if ret != 0 {
                fs::copy(&src_path, &dst_path)
                    .map_err(|e| format!("copy fallback: {}", e))?;
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn apfs_clone(_config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    Err("APFS clonefile is only available on macOS".into())
}

// ─── Hardlink Tree Driver ───────────────────────────────────────────

fn hardlink_tree(config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    let sandbox = create_sandbox_dir()?;
    hardlink_copy_tree(&config.source, &sandbox, config)?;
    Ok(SandboxHandle::new(sandbox, MaterializationDriver::HardlinkTree))
}

fn hardlink_copy_tree(
    source: &Path,
    dest: &Path,
    config: &MaterializeConfig,
) -> Result<(), String> {
    let entries = fs::read_dir(source)
        .map_err(|e| format!("read_dir: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry: {}", e))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let src_path = entry.path();
        let dst_path = dest.join(&*name);

        if should_skip(&name, config) {
            continue;
        }

        let file_type = entry.file_type().map_err(|e| format!("file_type: {}", e))?;

        if file_type.is_symlink() {
            recreate_symlink(&src_path, &dst_path)?;
        } else if file_type.is_dir() {
            if is_artifact_dir(&name, config) {
                fs::create_dir_all(&dst_path)
                    .map_err(|e| format!("mkdir: {}", e))?;
            } else {
                fs::create_dir_all(&dst_path)
                    .map_err(|e| format!("mkdir: {}", e))?;
                hardlink_copy_tree(&src_path, &dst_path, config)?;
            }
        } else if file_type.is_file() {
            fs::hard_link(&src_path, &dst_path)
                .map_err(|e| format!("hardlink {} failed: {}", name, e))?;
        }
    }
    Ok(())
}

// ─── CoW Clone Driver (Linux) ───────────────────────────────────────

#[cfg(target_os = "linux")]
fn cow_clone(_config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    Err("CoW FICLONE not yet implemented".into())
}

#[cfg(not(target_os = "linux"))]
fn cow_clone(_config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    Err("CoW FICLONE is only available on Linux".into())
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Helper: create a realistic source workspace in a temp dir.
    fn create_test_workspace() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "crow_test_src_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules/lodash")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();

        let mut f = fs::File::create(root.join("src/main.rs")).unwrap();
        f.write_all(b"fn main() { println!(\"hello\"); }").unwrap();

        let mut f = fs::File::create(root.join("Cargo.toml")).unwrap();
        f.write_all(b"[package]\nname = \"test\"").unwrap();

        let mut f = fs::File::create(root.join("node_modules/lodash/index.js")).unwrap();
        f.write_all(b"module.exports = {}").unwrap();

        // Add a symlink inside the workspace
        #[cfg(unix)]
        std::os::unix::fs::symlink("src/main.rs", root.join("main_link.rs")).unwrap();

        root
    }

    fn default_config(source: &Path) -> MaterializeConfig {
        MaterializeConfig {
            source: source.to_path_buf(),
            artifact_dirs: vec!["node_modules".into()],
            skip_patterns: vec![".git".into()],
            allow_hardlinks: false,
        }
    }

    fn hardlink_config(source: &Path) -> MaterializeConfig {
        MaterializeConfig {
            source: source.to_path_buf(),
            artifact_dirs: vec!["node_modules".into()],
            skip_patterns: vec![".git".into()],
            allow_hardlinks: true,
        }
    }

    // ─── INVARIANT: source is NEVER modified ────────────────────────

    #[test]
    fn safe_copy_never_modifies_source() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let src_main = fs::read_to_string(workspace.join("src/main.rs")).unwrap();
        let src_cargo = fs::read_to_string(workspace.join("Cargo.toml")).unwrap();

        let handle = safe_copy(&config).unwrap();

        // Modify a file inside the sandbox
        fs::write(handle.path().join("src/main.rs"), b"fn main() { panic!(); }").unwrap();

        // Source must be unchanged
        assert_eq!(fs::read_to_string(workspace.join("src/main.rs")).unwrap(), src_main);
        assert_eq!(fs::read_to_string(workspace.join("Cargo.toml")).unwrap(), src_cargo);

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── P1-1 FIX: artifact_dirs are empty dirs, NOT symlinks ───────

    #[test]
    fn artifact_dirs_are_empty_not_symlinked() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let handle = safe_copy(&config).unwrap();
        let nm_path = handle.path().join("node_modules");

        // node_modules must exist as a real directory, not a symlink
        assert!(nm_path.exists(), "node_modules should exist in sandbox");
        assert!(
            nm_path.symlink_metadata().unwrap().file_type().is_dir(),
            "node_modules must be a real directory, not a symlink"
        );
        assert!(
            !nm_path.symlink_metadata().unwrap().file_type().is_symlink(),
            "node_modules must NOT be a symlink"
        );

        // It must be empty — no content copied from source
        let entries: Vec<_> = fs::read_dir(&nm_path).unwrap().collect();
        assert!(entries.is_empty(), "artifact dir must be empty in sandbox");

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    #[test]
    fn writing_to_sandbox_artifact_dir_does_not_pollute_source() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let handle = safe_copy(&config).unwrap();

        // Write a new file inside the sandbox's node_modules
        let new_file = handle.path().join("node_modules/new_package.json");
        fs::write(&new_file, b"{}").unwrap();

        // Source node_modules must NOT contain the new file
        assert!(
            !workspace.join("node_modules/new_package.json").exists(),
            "writing to sandbox artifact dir must not pollute source"
        );

        // Source still has its original content
        assert!(workspace.join("node_modules/lodash/index.js").exists());

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── skip_patterns are excluded ─────────────────────────────────

    #[test]
    fn git_dir_is_skipped() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let handle = safe_copy(&config).unwrap();
        assert!(!handle.path().join(".git").exists(), ".git should not exist in sandbox");

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── Normal files are copied correctly ──────────────────────────

    #[test]
    fn normal_files_are_copied() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let handle = safe_copy(&config).unwrap();

        let content = fs::read_to_string(handle.path().join("src/main.rs")).unwrap();
        assert_eq!(content, "fn main() { println!(\"hello\"); }");

        let cargo = fs::read_to_string(handle.path().join("Cargo.toml")).unwrap();
        assert_eq!(cargo, "[package]\nname = \"test\"");

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── P2 FIX: symlinks are preserved ─────────────────────────────

    #[cfg(unix)]
    #[test]
    fn safe_copy_preserves_symlinks() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let handle = safe_copy(&config).unwrap();
        let link = handle.path().join("main_link.rs");

        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "symlink should be preserved in sandbox"
        );
        let target = fs::read_link(&link).unwrap();
        assert_eq!(target, PathBuf::from("src/main.rs"));

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── P1-2: HardlinkTree explicit opt-in ─────────────────────────

    #[test]
    fn hardlink_requires_opt_in() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace); // allow_hardlinks = false

        let handle = crate::materialize(&config).unwrap();
        // On macOS: should be ApfsClone or SafeCopy, never HardlinkTree
        assert_ne!(
            handle.driver(),
            MaterializationDriver::HardlinkTree,
            "HardlinkTree must not be used without opt-in"
        );

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    #[test]
    fn hardlink_opt_in_write_with_unlink_is_safe() {
        let workspace = create_test_workspace();
        let config = hardlink_config(&workspace);

        let src_content = fs::read_to_string(workspace.join("src/main.rs")).unwrap();

        let handle = hardlink_tree(&config).unwrap();
        assert_eq!(handle.driver(), MaterializationDriver::HardlinkTree);

        // The correct protocol: unlink then write
        let sandbox_file = handle.path().join("src/main.rs");
        fs::remove_file(&sandbox_file).unwrap();
        fs::write(&sandbox_file, b"fn main() { panic!(); }").unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            src_content,
            "hardlink unlink-then-write must not pollute source"
        );

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    #[cfg(unix)]
    #[test]
    fn hardlink_preserves_symlinks() {
        let workspace = create_test_workspace();
        let config = hardlink_config(&workspace);

        let handle = hardlink_tree(&config).unwrap();
        let link = handle.path().join("main_link.rs");

        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "hardlink driver should preserve symlinks"
        );

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── Top-level materialize still works ──────────────────────────

    #[test]
    fn materialize_succeeds_via_fallback() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let handle = crate::materialize(&config).unwrap();
        assert!(handle.path().exists());
        assert!(handle.path().join("src/main.rs").exists());

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }
}
