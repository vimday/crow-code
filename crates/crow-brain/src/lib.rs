//! crow-brain: Intent compiler, budget governor, and dual-track solver.
//!
//! Single-track Proposal Engine for simple tasks; multi-track MCTS
//! search policy for high-risk refactors. All governed by a strict
//! compute/time budget.

pub mod client;
pub mod compiler;
pub mod schema;

pub use client::ReqwestLlmClient;
pub use compiler::{CompilerError, IntentCompiler, LlmClient};
