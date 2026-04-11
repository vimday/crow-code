//! crow-materialize: OS-level sandbox materialization engine.
//!
//! Responsible for creating isolated copies of the user workspace
//! using the fastest available strategy, with safe fallback to full copies.
//!
//! # Invariants
//!
//! - The source workspace is **never modified**. This is the hardest rule.
//! - Build artifact directories (`node_modules`, `target/`) are never copied;
//!   they are mounted via read-only symlinks.
//! - Cleanup is automatic on [`SandboxHandle`] drop.

pub mod driver;
pub mod types;

pub use types::*;
