//! crow-materialize: Workspace-isolation materialization engine.
//!
//! Responsible for creating isolated copies of the user workspace
//! using the fastest available strategy, with safe fallback to full copies.
//!
//! # Invariants
//!
//! - The source workspace is **never modified**. This is the hardest rule.
//! - Build artifact directories are handled with a hybrid strategy:
//!   read-only dependency dirs (`node_modules`, `.venv`) are **symlinked**
//!   back to source; write-heavy build output dirs (`target`) are created
//!   as **empty directories** and redirected via env vars (e.g. `CARGO_TARGET_DIR`).
//! - Cleanup is automatic on [`SandboxHandle`] drop.

pub mod driver;
pub mod types;

pub use types::*;
