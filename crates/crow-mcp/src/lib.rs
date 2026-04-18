//! Model Context Protocol (MCP) Client Implementation.
//!
//! Provides a standardized transport implementation and client interface
//! for interacting with remote MCP servers out-of-process.

pub mod client;
pub mod transport;
pub mod types;

pub use client::McpClient;
pub use transport::StdioTransport;
