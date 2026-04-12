//! crow-intel: Codebase intelligence via AST analysis.
//!
//! Provides Tree-sitter outlines, LSP bridge, and per-language
//! confidence tiers for grading the reliability of gathered context.

pub mod skeleton;
pub use skeleton::{ASTProcessor, SupportedLanguage};
