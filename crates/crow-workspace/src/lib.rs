//! crow-workspace: Plan hydration and sandbox mutation applier.
//!
//! Bridges the gap between the LLM's intent (`crow-patch::IntentPlan`)
//! and physical filesystem mutations inside a materialized sandbox.
//! The hydrator injects ground-truth preconditions; the applier executes
//! edits with full precondition enforcement and drift-tolerant hunk matching.

pub mod applier;
pub mod hydrator;

pub use hydrator::{HydrationError, PlanHydrator};
