//! crow-materialize: Workspace-isolation materialization engine.
//!
//! Responsible for creating isolated copies of the user workspace
//! using the fastest available strategy, with safe fallback to full copies.
//!
//! # Invariants
//!
//! - The source workspace is **never modified**. This is the hardest rule.
//! - Build artifact directories (`node_modules`, `target/`) are created as
//!   **empty directories** so build tools regenerate them without writing
//!   through to the source.
//! - Cleanup is automatic on [`SandboxHandle`] drop.

pub mod driver;
pub mod types;

pub use types::*;
