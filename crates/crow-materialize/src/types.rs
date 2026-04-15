//! Core types for the materialization engine.

use crate::driver;
use std::fmt;
use std::path::{Path, PathBuf};

// ─── Materialization Driver ─────────────────────────────────────────

/// The strategy used to materialize a sandbox.
/// Listed in preference order: first available driver wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializationDriver {
    /// macOS APFS: `clonefile(2)` — O(1) zero-copy clone.
    ApfsClone,
    /// Linux Btrfs/ZFS: `FICLONE` ioctl — copy-on-write.
    CowClone,
    /// Cross-platform: hardlink tree.
    ///
    /// # Safety
    ///
    /// Hardlinks share inodes with the source. Any write to a hardlinked
    /// file mutates the source unless the caller unlinks first. This
    /// driver is **not included** in the default fallback chain and must
    /// be explicitly opted into via `MaterializeConfig::allow_hardlinks`.
    HardlinkTree,
    /// Final fallback: full recursive copy. Always correct, never fast.
    SafeCopy,
}

impl MaterializationDriver {
    /// Return the ranked list of *safe* drivers for this platform.
    ///
    /// `HardlinkTree` is **excluded** by default because it cannot
    /// enforce write isolation without a copy-up layer. Pass
    /// `allow_hardlinks = true` via config to include it.
    pub fn platform_preference(allow_hardlinks: bool) -> Vec<Self> {
        let mut drivers = Vec::new();

        #[cfg(target_os = "macos")]
        drivers.push(Self::ApfsClone);

        #[cfg(target_os = "linux")]
        drivers.push(Self::CowClone);

        if allow_hardlinks {
            drivers.push(Self::HardlinkTree);
        }

        drivers.push(Self::SafeCopy);

        drivers
    }
}

impl fmt::Display for MaterializationDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApfsClone => write!(f, "APFS clonefile"),
            Self::CowClone => write!(f, "CoW FICLONE"),
            Self::HardlinkTree => write!(f, "hardlink tree"),
            Self::SafeCopy => write!(f, "safe copy"),
        }
    }
}

// ─── Configuration ──────────────────────────────────────────────────

/// Configuration for a materialization request.
/// Decoupled from `crow-probe` types — accepts plain strings to avoid
/// cross-crate coupling at L1.
#[derive(Debug, Clone)]
pub struct MaterializeConfig {
    /// Source workspace root (absolute path).
    pub source: PathBuf,
    /// Directory names that are build artifacts (e.g. "node_modules", "target").
    /// These are created as **empty directories** in the sandbox so build
    /// tools can regenerate them without writing through to the source.
    /// A shared read-only cache strategy is planned but not yet safe.
    pub artifact_dirs: Vec<String>,
    /// Glob patterns for entries to skip entirely (e.g. ".git", "*.swp").
    /// Matched against each entry's basename via `globset::GlobSet`.
    pub skip_patterns: Vec<String>,
    /// If true, include HardlinkTree in the driver fallback chain.
    /// Callers MUST use unlink-before-write discipline with hardlinked
    /// sandboxes. Default: false.
    pub allow_hardlinks: bool,
}

// ─── Sandbox Handle ─────────────────────────────────────────────────

/// A live handle to a materialized sandbox.
///
/// The sandbox directory is automatically cleaned up when this handle
/// is dropped (unless `into_path()` is called to take ownership).
#[derive(Debug)]
pub struct SandboxHandle {
    /// Absolute path to the sandbox root.
    path: PathBuf,
    /// Which driver was used to create it.
    driver: MaterializationDriver,
    /// Whether cleanup should happen on drop.
    owned: bool,
}

impl SandboxHandle {
    pub(crate) fn new(path: PathBuf, driver: MaterializationDriver) -> Self {
        Self {
            path,
            driver,
            owned: true,
        }
    }

    /// The sandbox root directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Which materialization strategy was used.
    pub fn driver(&self) -> MaterializationDriver {
        self.driver
    }

    /// Take ownership of the path, preventing automatic cleanup.
    pub fn into_path(mut self) -> PathBuf {
        self.owned = false;
        self.path.clone()
    }

    /// Create a non-owning view of this sandbox for use in blocking tasks.
    ///
    /// The returned handle points to the same path and reports the same driver,
    /// but will **not** clean up the directory on drop. The original handle
    /// retains ownership and cleanup responsibility.
    pub fn non_owning_view(&self) -> Self {
        Self {
            path: self.path.clone(),
            driver: self.driver,
            owned: false,
        }
    }
}

impl Drop for SandboxHandle {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

// ─── Error ──────────────────────────────────────────────────────────

/// Errors that can occur during materialization.
#[derive(Debug)]
pub enum MaterializeError {
    /// Source directory does not exist or is not a directory.
    SourceNotFound(PathBuf),
    /// All drivers failed. Contains the last error.
    AllDriversFailed {
        attempted: Vec<MaterializationDriver>,
        last_error: String,
    },
    /// IO error during materialization.
    Io(std::io::Error),
}

impl fmt::Display for MaterializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceNotFound(p) => write!(f, "source not found: {}", p.display()),
            Self::AllDriversFailed {
                attempted,
                last_error,
            } => {
                write!(f, "all drivers failed ({:?}): {}", attempted, last_error)
            }
            Self::Io(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for MaterializeError {}

impl From<std::io::Error> for MaterializeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ─── Public API ─────────────────────────────────────────────────────

/// Materialize a sandbox from the given configuration.
///
/// Tries drivers in platform preference order. The first driver that
/// succeeds wins; if all fail, returns `AllDriversFailed`.
///
/// `HardlinkTree` is only attempted if `config.allow_hardlinks` is true.
pub fn materialize(config: &MaterializeConfig) -> Result<SandboxHandle, MaterializeError> {
    if !config.source.is_dir() {
        return Err(MaterializeError::SourceNotFound(config.source.clone()));
    }

    let drivers = MaterializationDriver::platform_preference(config.allow_hardlinks);
    let mut last_error = String::new();

    for driver in &drivers {
        match driver::execute(driver, config) {
            Ok(handle) => return Ok(handle),
            Err(e) => {
                last_error = format!("{}: {}", driver, e);
            }
        }
    }

    Err(MaterializeError::AllDriversFailed {
        attempted: drivers,
        last_error,
    })
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preference_excludes_hardlink() {
        let prefs = MaterializationDriver::platform_preference(false);
        assert!(!prefs.contains(&MaterializationDriver::HardlinkTree));
        assert!(prefs.contains(&MaterializationDriver::SafeCopy));
        assert_eq!(prefs.last(), Some(&MaterializationDriver::SafeCopy));
    }

    #[test]
    fn opt_in_preference_includes_hardlink() {
        let prefs = MaterializationDriver::platform_preference(true);
        assert!(prefs.contains(&MaterializationDriver::HardlinkTree));
        // HardlinkTree should come before SafeCopy
        let hl_pos = prefs
            .iter()
            .position(|d| *d == MaterializationDriver::HardlinkTree)
            .unwrap();
        let sc_pos = prefs
            .iter()
            .position(|d| *d == MaterializationDriver::SafeCopy)
            .unwrap();
        assert!(hl_pos < sc_pos);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_prefers_apfs_clone() {
        let prefs = MaterializationDriver::platform_preference(false);
        assert_eq!(prefs[0], MaterializationDriver::ApfsClone);
    }

    #[test]
    fn source_not_found_error() {
        let config = MaterializeConfig {
            source: PathBuf::from("/nonexistent/path/that/does/not/exist"),
            artifact_dirs: vec![],
            skip_patterns: vec![],
            allow_hardlinks: false,
        };
        let result = materialize(&config);
        assert!(result.is_err());
        match result.unwrap_err() {
            MaterializeError::SourceNotFound(_) => {}
            other => panic!("expected SourceNotFound, got: {:?}", other),
        }
    }

    #[test]
    fn sandbox_handle_cleans_up_on_drop() {
        let tmp = std::env::temp_dir().join("crow_test_cleanup");
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(tmp.exists());

        {
            let _handle = SandboxHandle::new(tmp.clone(), MaterializationDriver::SafeCopy);
        }

        assert!(!tmp.exists(), "sandbox should be cleaned up after drop");
    }

    #[test]
    fn sandbox_handle_into_path_prevents_cleanup() {
        let tmp = std::env::temp_dir().join("crow_test_no_cleanup");
        std::fs::create_dir_all(&tmp).unwrap();

        let handle = SandboxHandle::new(tmp.clone(), MaterializationDriver::SafeCopy);
        let path = handle.into_path();

        assert!(path.exists(), "path should still exist after into_path");
        let _ = std::fs::remove_dir_all(&path);
    }
}
