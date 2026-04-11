//! crow-materialize: OS-level sandbox materialization engine.
//!
//! Responsible for creating isolated copies of the user workspace
//! using the fastest available strategy (APFS clonefile, Btrfs CoW,
//! hardlink trees) with safe fallback to full copies.
//!
//! Build artifact directories (node_modules, target/) are never copied;
//! they are mounted via read-only symlinks.
