//! crow-verifier: Sandbox command execution and ACI log truncation.
//!
//! Runs build/test commands inside materialized sandboxes. Strictly
//! truncates unbounded stdout/stderr into Head + Tail windows to
//! protect the LLM token budget.
//!
//! # Architecture
//!
//! This crate sits at **Layer 2 (Crucible)**. It depends on:
//! - `crow-evidence` (L0): `TestRun`, `TestOutcome`
//! - `crow-probe` (L0): `VerificationCommand`
//!
//! It does NOT depend on `crow-materialize` — it receives a sandbox
//! path, keeping the coupling minimal.

pub mod aci;
pub mod executor;
pub mod types;

pub use types::*;
