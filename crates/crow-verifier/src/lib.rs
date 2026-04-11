//! crow-verifier: Sandbox command execution and ACI log truncation.
//!
//! Runs build/test commands inside materialized sandboxes. Strictly
//! truncates unbounded stdout/stderr into <Header 50 lines> + <Tail 150
//! lines> to protect the LLM token budget.
