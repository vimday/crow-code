//! Runtime orchestration for Crow Code.
//!
//! This crate contains the main conversation loop, epistemic reasoning,
//! subagent task management, and task registries.
pub mod budget;
pub mod cancel;
pub mod context;
pub mod epistemic;
pub mod event;
pub mod file_state;
pub mod mcp;
pub mod registry;
pub mod session;
pub mod subagent;
