//! crow-workspace: Event-sourcing log and O(1) snapshot state machine.
//!
//! Maintains the canonical event ledger for all workspace mutations.
//! Supports checkpointing, compaction, and deterministic replay.
