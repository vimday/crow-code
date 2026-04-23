//! crow-brain: Intent compiler, budget governor, and dual-track solver.
//!
//! Single-track Proposal Engine for simple tasks; multi-track MCTS
//! search policy for high-risk refactors. All governed by a strict
//! compute/time budget.

pub mod anthropic;
pub mod autodream;
pub mod client;
pub mod compactor;
pub mod compiler;
pub mod router;
pub mod schema;
pub mod skill;

pub mod usage;

pub use client::{BrainError, LlmProviderConfig, ProviderKind, ReqwestLlmClient};
pub use compiler::{
    AgentResponse, AgentResponseBlock, ChatMessage, ChatRole, CompilerError,
    IntentCompiler, LlmClient, ToolCallRequest, ToolStreamObserver,
};
pub use router::{build_client, describe_provider};
pub use usage::TokenUsage;
