//! Materialization driver implementations.
//!
//! Each driver implements the same contract: given a source directory and
//! config, produce an isolated sandbox directory. The source must NEVER
//! be modified.

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
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let sandbox_path = std::env::temp_dir().join(format!("crow_vfs_{}", id));
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
/// Currently supports exact filename matches only. Glob expansion is
/// deferred to a future step.
fn should_skip(name: &str, config: &MaterializeConfig) -> bool {
    config.skip_patterns.iter().any(|p| {
        // Strip trailing slash for directory pattern matching
        let pattern = p.trim_end_matches('/');
        name == pattern
    })
}

/// Recursively copy a directory tree, symlinking artifact dirs and
/// skipping ignored entries.
fn copy_tree(
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

        // Skip ignored entries entirely
        if should_skip(&name, config) {
            continue;
        }

        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type error: {}", e))?;

        if file_type.is_dir() {
            if is_artifact_dir(&name, config) {
                // Symlink artifact directories, never copy them
                #[cfg(unix)]
                std::os::unix::fs::symlink(&src_path, &dst_path)
                    .map_err(|e| format!("symlink {} failed: {}", name, e))?;
                #[cfg(not(unix))]
                return Err(format!("symlinks not supported on this platform for {}", name));
            } else {
                fs::create_dir_all(&dst_path)
                    .map_err(|e| format!("mkdir {} failed: {}", dst_path.display(), e))?;
                copy_tree(&src_path, &dst_path, config)?;
            }
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("copy {} failed: {}", name, e))?;
        } else if file_type.is_symlink() {
            // Preserve existing symlinks as-is
            let target = fs::read_link(&src_path)
                .map_err(|e| format!("readlink {} failed: {}", name, e))?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &dst_path)
                .map_err(|e| format!("symlink {} failed: {}", name, e))?;
        }
    }
    Ok(())
}

// ─── SafeCopy Driver ────────────────────────────────────────────────

fn safe_copy(config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    let sandbox = create_sandbox_dir()?;
    copy_tree(&config.source, &sandbox, config)?;
    Ok(SandboxHandle::new(sandbox, MaterializationDriver::SafeCopy))
}

// ─── APFS Clone Driver (macOS) ──────────────────────────────────────

#[cfg(target_os = "macos")]
fn apfs_clone(config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    // clonefile(2) works on individual files, not directories.
    // For a directory tree, we still walk and clone each file,
    // but artifact_dirs get symlinked.
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

        if file_type.is_dir() {
            if is_artifact_dir(&name, config) {
                std::os::unix::fs::symlink(&src_path, &dst_path)
                    .map_err(|e| format!("symlink: {}", e))?;
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
                // clonefile failed (e.g. cross-filesystem), fall back to copy
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

        if file_type.is_dir() {
            if is_artifact_dir(&name, config) {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&src_path, &dst_path)
                    .map_err(|e| format!("symlink: {}", e))?;
                #[cfg(not(unix))]
                return Err("symlinks not supported".into());
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
    // FICLONE ioctl implementation deferred to Step 3b
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

        root
    }

    fn default_config(source: &Path) -> MaterializeConfig {
        MaterializeConfig {
            source: source.to_path_buf(),
            artifact_dirs: vec!["node_modules".into()],
            skip_patterns: vec![".git".into()],
        }
    }

    // ─── INVARIANT: source is NEVER modified ────────────────────────

    #[test]
    fn safe_copy_never_modifies_source() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        // Record source state before materialization
        let src_main = fs::read_to_string(workspace.join("src/main.rs")).unwrap();
        let src_cargo = fs::read_to_string(workspace.join("Cargo.toml")).unwrap();

        let handle = safe_copy(&config).unwrap();

        // Modify a file inside the sandbox
        let sandbox_main = handle.path().join("src/main.rs");
        fs::write(&sandbox_main, b"fn main() { panic!(); }").unwrap();

        // Source must be unchanged
        assert_eq!(fs::read_to_string(workspace.join("src/main.rs")).unwrap(), src_main);
        assert_eq!(fs::read_to_string(workspace.join("Cargo.toml")).unwrap(), src_cargo);

        // Cleanup
        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── artifact_dirs are symlinked, not copied ────────────────────

    #[test]
    fn artifact_dirs_are_symlinked() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let handle = safe_copy(&config).unwrap();
        let nm_path = handle.path().join("node_modules");

        assert!(nm_path.exists(), "node_modules should exist in sandbox");
        assert!(
            nm_path.symlink_metadata().unwrap().file_type().is_symlink(),
            "node_modules should be a symlink"
        );

        // The symlink should point to the original
        let target = fs::read_link(&nm_path).unwrap();
        assert_eq!(target, workspace.join("node_modules"));

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── skip_patterns are excluded ─────────────────────────────────

    #[test]
    fn git_dir_is_skipped() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let handle = safe_copy(&config).unwrap();
        let git_path = handle.path().join(".git");

        assert!(!git_path.exists(), ".git should not exist in sandbox");

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

    // ─── Hardlink driver: source isolation after unlink ─────────────

    #[test]
    fn hardlink_write_does_not_pollute_source() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let src_content = fs::read_to_string(workspace.join("src/main.rs")).unwrap();

        let handle = hardlink_tree(&config).unwrap();

        // CRITICAL: For hardlinks, writing directly would modify the source.
        // The correct pattern is: unlink (remove), then write new file.
        let sandbox_file = handle.path().join("src/main.rs");
        fs::remove_file(&sandbox_file).unwrap(); // unlink the hardlink
        fs::write(&sandbox_file, b"fn main() { panic!(); }").unwrap();

        // Source must be unchanged
        assert_eq!(
            fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            src_content,
            "hardlink unlink-then-write must not pollute source"
        );

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── Materialization top-level fallback ─────────────────────────

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
