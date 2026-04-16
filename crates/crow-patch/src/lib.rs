//! crow-patch: Unified patch contract.
//!
//! The LLM never writes to disk directly. All intended mutations are
//! compiled into an [`IntentPlan`] containing atomic [`EditOp`]s with
//! preconditions, base snapshot anchoring, and fuzzy-match hunks.

pub mod types;
pub mod util;

pub use types::*;
pub use util::safe_truncate;
