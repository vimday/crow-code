//! crow-verifier: Workspace isolated command execution and ACI log truncation.
//!
//! Runs build/test commands inside newly materialized workspace isolates.
//! truncates unbounded stdout/stderr into Head + Tail windows to
//! protect the LLM token budget.
//!
//! # Architecture
//!
//! This crate sits at **Layer 2 (Crucible)**. It depends on:
//! - `crow-evidence` (L0): `TestRun`, `TestOutcome`
//! - `crow-probe` (L0): `VerificationCommand`
//!
//! It does NOT depend on `crow-materialize` — it receives an isolated
//! path, keeping the coupling minimal.
//!
//! # Limitations
//!
//! This crate does NOT provide true OS-level security sandboxing (such as
//! `bwrap`, `nsjail`, or macOS `seatbelt`). It guarantees that operations
//! will not pollute the parent workspace and boundaries do not trivially
//! escape `cwd`, but a malicious command could still read the global filesystem
//! or execute arbitrary network requests.

pub mod aci;
pub mod executor;
pub mod preflight;
pub mod types;

pub use types::*;
