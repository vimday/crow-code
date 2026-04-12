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
//! - Source symlinks that resolve within the workspace are preserved.
//!   Absolute or out-of-bounds symlinks are rejected.

use crate::types::{MaterializationDriver, MaterializeConfig, SandboxHandle};
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

// ─── Temp directory guard (P2 fix) ──────────────────────────────────

/// RAII guard for sandbox directories under construction.
/// If the materialization fails partway, the directory is cleaned up
/// on drop. Call `into_handle()` to promote to a permanent SandboxHandle.
struct SandboxGuard {
    path: PathBuf,
    disarmed: bool,
}

impl SandboxGuard {
    fn new() -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("crow_vfs_{}_{}", id, seq));
        fs::create_dir_all(&path).map_err(|e| format!("failed to create sandbox dir: {}", e))?;
        Ok(Self {
            path,
            disarmed: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Promote to SandboxHandle, disarming the cleanup guard.
    fn into_handle(mut self, driver: MaterializationDriver) -> SandboxHandle {
        self.disarmed = true;
        SandboxHandle::new(self.path.clone(), driver)
    }
}

impl Drop for SandboxGuard {
    fn drop(&mut self) {
        if !self.disarmed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────

/// Check if a directory name matches any artifact_dirs entry.
fn is_artifact_dir(name: &str, config: &MaterializeConfig) -> bool {
    config.artifact_dirs.iter().any(|a| a == name)
}

fn build_skip_set(config: &MaterializeConfig) -> Result<globset::GlobSet, String> {
    let mut builder = globset::GlobSetBuilder::new();
    for p in &config.skip_patterns {
        let pattern = p.trim_end_matches('/');
        let glob = globset::Glob::new(pattern)
            .map_err(|e| format!("invalid glob pattern '{}': {}", pattern, e))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| format!("failed to build globset: {}", e))
}

/// Check if an entry matches any skip pattern.
/// Matches against both the basename (for simple patterns like ".git")
/// and the relative path from the workspace root (for path-contextual
/// patterns like "**/dist" or "src/generated/**").
fn should_skip(basename: &str, rel_path: &std::path::Path, skip_set: &globset::GlobSet) -> bool {
    skip_set.is_match(basename) || skip_set.is_match(rel_path)
}

/// Validate and re-create a symlink in the sandbox.
///
/// # Safety rules (P1 fix)
///
/// - **Absolute symlinks** are rejected outright — they bypass the sandbox.
/// - **Relative symlinks** are resolved against the link's parent directory
///   in the source tree. If the resolved path escapes the workspace root,
///   the link is rejected.
/// - Only symlinks that stay within the workspace boundary are recreated.
fn recreate_symlink(src_path: &Path, dst_path: &Path, workspace_root: &Path) -> Result<(), String> {
    let target = fs::read_link(src_path)
        .map_err(|e| format!("readlink {} failed: {}", src_path.display(), e))?;

    // Reject absolute symlinks entirely
    if target.is_absolute() {
        return Err(format!(
            "rejecting absolute symlink {} -> {} (would escape sandbox)",
            src_path.display(),
            target.display()
        ));
    }

    // Resolve the relative target against the link's parent dir.
    // Canonicalize link_parent first so that symlinked workspace roots
    // don't cause false out-of-bounds rejections (P2 fix).
    let link_parent = src_path.parent().unwrap_or(workspace_root);
    let canonical_parent = link_parent
        .canonicalize()
        .unwrap_or_else(|_| link_parent.to_path_buf());
    let resolved = canonical_parent.join(&target);

    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

    // Canonicalize to resolve ../ components, then check bounds.
    // If the target doesn't exist yet, normalize both sides lexically
    // to keep the comparison symmetric.
    let canonical = if resolved.exists() {
        resolved
            .canonicalize()
            .map_err(|e| format!("canonicalize {} failed: {}", resolved.display(), e))?
    } else {
        normalize_path(&resolved)
    };

    if !canonical.starts_with(&canonical_root) {
        return Err(format!(
            "rejecting out-of-bounds symlink {} -> {} (resolves to {}, outside workspace {})",
            src_path.display(),
            target.display(),
            canonical.display(),
            canonical_root.display()
        ));
    }

    // Safe to recreate: target stays within workspace boundary
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, dst_path)
            .map_err(|e| format!("symlink {} failed: {}", dst_path.display(), e))?;
    }
    #[cfg(windows)]
    {
        if src_path.is_dir() {
            std::os::windows::fs::symlink_dir(&target, dst_path)
                .map_err(|e| format!("symlink_dir {} failed: {}", dst_path.display(), e))?;
        } else {
            std::os::windows::fs::symlink_file(&target, dst_path)
                .map_err(|e| format!("symlink_file {} failed: {}", dst_path.display(), e))?;
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        return Err("symlink preservation not yet supported on this platform".into());
    }
    Ok(())
}

/// Best-effort path normalization without filesystem access.
/// Resolves `.` and `..` components lexically.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Only pop if we have a normal component to pop
                if matches!(components.last(), Some(std::path::Component::Normal(_))) {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            std::path::Component::CurDir => {} // skip
            _ => components.push(component),
        }
    }
    components.iter().collect()
}

// ─── SafeCopy Driver ────────────────────────────────────────────────

fn safe_copy(config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    let skip_set = build_skip_set(config)?;
    let guard = SandboxGuard::new()?;
    safe_copy_tree(
        &config.source,
        guard.path(),
        config,
        &skip_set,
        Path::new(""),
    )?;
    Ok(guard.into_handle(MaterializationDriver::SafeCopy))
}

fn safe_copy_tree(
    source: &Path,
    dest: &Path,
    config: &MaterializeConfig,
    skip_set: &globset::GlobSet,
    rel_prefix: &Path,
) -> Result<(), String> {
    let entries =
        fs::read_dir(source).map_err(|e| format!("failed to read {}: {}", source.display(), e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry error: {}", e))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let src_path = entry.path();
        let dst_path = dest.join(&*name);
        let rel_path = rel_prefix.join(&*name);

        if should_skip(&name, &rel_path, skip_set) {
            continue;
        }

        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type error: {}", e))?;

        if file_type.is_symlink() {
            recreate_symlink(&src_path, &dst_path, &config.source)?;
        } else if file_type.is_dir() {
            if is_artifact_dir(&name, config) {
                fs::create_dir_all(&dst_path)
                    .map_err(|e| format!("mkdir {} failed: {}", dst_path.display(), e))?;
            } else {
                fs::create_dir_all(&dst_path)
                    .map_err(|e| format!("mkdir {} failed: {}", dst_path.display(), e))?;
                safe_copy_tree(&src_path, &dst_path, config, skip_set, &rel_path)?;
            }
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).map_err(|e| format!("copy {} failed: {}", name, e))?;
        }
    }
    Ok(())
}

// ─── APFS Clone Driver (macOS) ──────────────────────────────────────

#[cfg(target_os = "macos")]
fn apfs_clone(config: &MaterializeConfig) -> Result<SandboxHandle, String> {
    let skip_set = build_skip_set(config)?;
    let guard = SandboxGuard::new()?;
    apfs_clone_tree(
        &config.source,
        guard.path(),
        config,
        &skip_set,
        Path::new(""),
    )?;
    Ok(guard.into_handle(MaterializationDriver::ApfsClone))
}

#[cfg(target_os = "macos")]
fn apfs_clone_tree(
    source: &Path,
    dest: &Path,
    config: &MaterializeConfig,
    skip_set: &globset::GlobSet,
    rel_prefix: &Path,
) -> Result<(), String> {
    let entries = fs::read_dir(source).map_err(|e| format!("read_dir: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry: {}", e))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let src_path = entry.path();
        let dst_path = dest.join(&*name);
        let rel_path = rel_prefix.join(&*name);

        if should_skip(&name, &rel_path, skip_set) {
            continue;
        }

        let file_type = entry.file_type().map_err(|e| format!("file_type: {}", e))?;

        if file_type.is_symlink() {
            recreate_symlink(&src_path, &dst_path, &config.source)?;
        } else if file_type.is_dir() {
            if is_artifact_dir(&name, config) {
                fs::create_dir_all(&dst_path).map_err(|e| format!("mkdir: {}", e))?;
            } else {
                fs::create_dir_all(&dst_path).map_err(|e| format!("mkdir: {}", e))?;
                apfs_clone_tree(&src_path, &dst_path, config, skip_set, &rel_path)?;
            }
        } else if file_type.is_file() {
            let src_c = std::ffi::CString::new(src_path.to_string_lossy().as_bytes())
                .map_err(|_| "invalid path for clonefile")?;
            let dst_c = std::ffi::CString::new(dst_path.to_string_lossy().as_bytes())
                .map_err(|_| "invalid path for clonefile")?;
            let ret = unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
            if ret != 0 {
                fs::copy(&src_path, &dst_path).map_err(|e| format!("copy fallback: {}", e))?;
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
    let skip_set = build_skip_set(config)?;
    let guard = SandboxGuard::new()?;
    hardlink_copy_tree(
        &config.source,
        guard.path(),
        config,
        &skip_set,
        Path::new(""),
    )?;
    Ok(guard.into_handle(MaterializationDriver::HardlinkTree))
}

fn hardlink_copy_tree(
    source: &Path,
    dest: &Path,
    config: &MaterializeConfig,
    skip_set: &globset::GlobSet,
    rel_prefix: &Path,
) -> Result<(), String> {
    let entries = fs::read_dir(source).map_err(|e| format!("read_dir: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry: {}", e))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let src_path = entry.path();
        let dst_path = dest.join(&*name);
        let rel_path = rel_prefix.join(&*name);

        if should_skip(&name, &rel_path, skip_set) {
            continue;
        }

        let file_type = entry.file_type().map_err(|e| format!("file_type: {}", e))?;

        if file_type.is_symlink() {
            recreate_symlink(&src_path, &dst_path, &config.source)?;
        } else if file_type.is_dir() {
            if is_artifact_dir(&name, config) {
                fs::create_dir_all(&dst_path).map_err(|e| format!("mkdir: {}", e))?;
            } else {
                fs::create_dir_all(&dst_path).map_err(|e| format!("mkdir: {}", e))?;
                hardlink_copy_tree(&src_path, &dst_path, config, skip_set, &rel_path)?;
            }
        } else if file_type.is_file() {
            if let Err(e) = fs::hard_link(&src_path, &dst_path) {
                fs::copy(&src_path, &dst_path)
                    .map_err(|ce| format!("hardlink fallback failed: {} (orig: {})", ce, e))?;
            }
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

    fn create_test_workspace() -> PathBuf {
        let root = std::env::temp_dir().join(format!("crow_test_src_{}", {
            use std::sync::atomic::{AtomicU64, Ordering};
            static C: AtomicU64 = AtomicU64::new(0);
            let id = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            format!("{}_{}", id, C.fetch_add(1, Ordering::Relaxed))
        }));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("node_modules/lodash")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();

        let mut f = fs::File::create(root.join("src/main.rs")).unwrap();
        f.write_all(b"fn main() { println!(\"hello\"); }").unwrap();

        let mut f = fs::File::create(root.join("Cargo.toml")).unwrap();
        f.write_all(b"[package]\nname = \"test\"").unwrap();

        let mut f = fs::File::create(root.join("node_modules/lodash/index.js")).unwrap();
        f.write_all(b"module.exports = {}").unwrap();

        // Safe relative symlink within workspace
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

        let handle = safe_copy(&config).unwrap();
        fs::write(
            handle.path().join("src/main.rs"),
            b"fn main() { panic!(); }",
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            src_main
        );

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── artifact_dirs are empty dirs ───────────────────────────────

    #[test]
    fn artifact_dirs_are_empty_not_symlinked() {
        let workspace = create_test_workspace();
        let config = default_config(&workspace);

        let handle = safe_copy(&config).unwrap();
        let nm_path = handle.path().join("node_modules");

        assert!(nm_path.exists());
        assert!(!nm_path.symlink_metadata().unwrap().file_type().is_symlink());
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
        fs::write(handle.path().join("node_modules/new.json"), b"{}").unwrap();

        assert!(!workspace.join("node_modules/new.json").exists());

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── skip_patterns ──────────────────────────────────────────────

    #[test]
    fn git_dir_is_skipped() {
        let workspace = create_test_workspace();
        let handle = safe_copy(&default_config(&workspace)).unwrap();
        assert!(!handle.path().join(".git").exists());
        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── Normal files ───────────────────────────────────────────────

    #[test]
    fn normal_files_are_copied() {
        let workspace = create_test_workspace();
        let handle = safe_copy(&default_config(&workspace)).unwrap();

        assert_eq!(
            fs::read_to_string(handle.path().join("src/main.rs")).unwrap(),
            "fn main() { println!(\"hello\"); }"
        );
        assert_eq!(
            fs::read_to_string(handle.path().join("Cargo.toml")).unwrap(),
            "[package]\nname = \"test\""
        );

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── P1 FIX: symlink boundary enforcement ──────────────────────

    #[cfg(unix)]
    #[test]
    fn safe_relative_symlink_is_preserved() {
        let workspace = create_test_workspace();
        let handle = safe_copy(&default_config(&workspace)).unwrap();
        let link = handle.path().join("main_link.rs");

        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("src/main.rs"));

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    #[cfg(unix)]
    #[test]
    fn absolute_symlink_is_rejected() {
        let workspace = create_test_workspace();
        // Create an absolute symlink pointing outside
        std::os::unix::fs::symlink("/etc/hosts", workspace.join("escape_abs")).unwrap();

        let config = default_config(&workspace);
        let result = safe_copy(&config);

        assert!(result.is_err(), "absolute symlink must be rejected");
        let err = result.unwrap_err();
        assert!(
            err.contains("absolute symlink"),
            "error should mention absolute symlink: {}",
            err
        );

        let _ = fs::remove_dir_all(&workspace);
    }

    #[cfg(unix)]
    #[test]
    fn out_of_bounds_relative_symlink_is_rejected() {
        let workspace = create_test_workspace();
        // Create a relative symlink that escapes the workspace via ../
        std::os::unix::fs::symlink("../../etc/passwd", workspace.join("escape_rel")).unwrap();

        let config = default_config(&workspace);
        let result = safe_copy(&config);

        assert!(result.is_err(), "out-of-bounds symlink must be rejected");
        let err = result.unwrap_err();
        assert!(
            err.contains("out-of-bounds"),
            "error should mention out-of-bounds: {}",
            err
        );

        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── P2 FIX: failed driver cleans up temp dir ───────────────────

    #[test]
    fn sandbox_guard_cleans_up_on_failure() {
        let guard = SandboxGuard::new().unwrap();
        let path = guard.path().to_path_buf();
        assert!(path.exists());

        // Drop without into_handle — simulates a failed materialization
        drop(guard);

        assert!(
            !path.exists(),
            "SandboxGuard must clean up on drop without into_handle"
        );
    }

    #[test]
    fn sandbox_guard_survives_into_handle() {
        let guard = SandboxGuard::new().unwrap();
        let path = guard.path().to_path_buf();

        let handle = guard.into_handle(MaterializationDriver::SafeCopy);
        assert!(path.exists(), "dir should survive into_handle");

        drop(handle);
        assert!(
            !path.exists(),
            "dir should be cleaned by SandboxHandle drop"
        );
    }

    // ─── HardlinkTree opt-in ────────────────────────────────────────

    #[test]
    fn hardlink_requires_opt_in() {
        let workspace = create_test_workspace();
        let handle = crate::materialize(&default_config(&workspace)).unwrap();
        assert_ne!(handle.driver(), MaterializationDriver::HardlinkTree);
        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    #[test]
    fn hardlink_opt_in_write_with_unlink_is_safe() {
        let workspace = create_test_workspace();
        let src_content = fs::read_to_string(workspace.join("src/main.rs")).unwrap();

        let handle = hardlink_tree(&hardlink_config(&workspace)).unwrap();
        let sandbox_file = handle.path().join("src/main.rs");
        fs::remove_file(&sandbox_file).unwrap();
        fs::write(&sandbox_file, b"fn main() { panic!(); }").unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            src_content
        );

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    #[cfg(unix)]
    #[test]
    fn hardlink_preserves_safe_symlinks() {
        let workspace = create_test_workspace();
        let handle = hardlink_tree(&hardlink_config(&workspace)).unwrap();
        let link = handle.path().join("main_link.rs");

        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());

        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }

    // ─── Top-level fallback ─────────────────────────────────────────

    #[test]
    fn materialize_succeeds_via_fallback() {
        let workspace = create_test_workspace();
        let handle = crate::materialize(&default_config(&workspace)).unwrap();
        assert!(handle.path().join("src/main.rs").exists());
        drop(handle);
        let _ = fs::remove_dir_all(&workspace);
    }
}
