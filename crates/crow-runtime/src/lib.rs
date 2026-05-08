//! Runtime orchestration for Crow Code.
//!
//! This crate contains the main conversation loop, epistemic reasoning,
//! subagent task management, and task registries.
pub mod agent_loop;
pub mod agent_status;
pub mod agents_md;
pub mod budget;
pub mod cancel;
pub mod context;
pub mod epistemic;
pub mod event;
pub mod file_state;
pub mod git_context;
pub mod mcp;
pub mod registry;
pub mod role;
pub mod session;
pub mod session_store;
pub mod subagent;
pub mod turn_context;
pub mod turn_diff;
pub mod turn_timing;

