//! crow-materialize: Workspace-isolation materialization engine.
//!
//! Responsible for creating isolated copies of the user workspace
//! using the fastest available strategy, with safe fallback to full copies.
//!
//! # Invariants
//!
//! - The source workspace is **never modified**. This is the hardest rule.
//! - Build artifact directories (`node_modules`, `target/`, `.venv`) are
//!   created as **empty directories** — never copied or symlinked from source.
//!   Dependency resolution is delegated to the verifier via environment variable
//!   injection (e.g. `CARGO_TARGET_DIR`, `NODE_PATH`).
//! - Cleanup is automatic on [`SandboxHandle`] drop.

pub mod driver;
pub mod types;

pub use types::*;
