//! crow-probe: Repository recon radar.
//!
//! Within the first 100ms of launch, probes the workspace to detect
//! primary language, build system, verification candidates, and
//! gitignore/filter rules. Outputs candidates with confidence, not
//! single answers.

pub mod scanner;
pub mod types;

pub use scanner::*;
pub use types::*;
