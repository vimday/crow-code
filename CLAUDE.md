# CLAUDE.md — Crow-Code Project Memory

This file is the persistent project context for AI assistants working on this codebase.

## Project Identity

- **Name:** Crow Code
- **Organization:** CorvusMatrix
- **Repo:** `crow-code`
- **Language:** Rust (pure workspace, no Go/Python/JS)
- **Binary:** `crow` (built from `crates/crow-cli`)
- **License:** MIT

## Architecture Overview

Crow Code is an evidence-driven AI coding agent built on defensive engineering
principles. See `docs/RFC-001-Architecture-Baseline.md` for the full
constitution.

### Crate Topology (dependency flows downward only)

```
Layer 0 — Currencies (zero inter-deps, zero external deps)
  crow-patch       Unified patch contract (EditOp, IntentPlan)
  crow-evidence    Multidimensional verification evidence (EvidenceMatrix)
  crow-probe       Repository recon radar (ProjectProfile)

Layer 1 — Runtime
  crow-workspace   Event-sourcing log & snapshot state machine
  crow-materialize OS-level sandbox materialization (CoW / symlink)

Layer 2 — Crucible
  crow-verifier    Sandbox command execution & ACI log truncation

Layer 3 — Intelligence
  crow-intel       Tree-sitter outlines, LSP bridge, language-tier confidence

Layer 4 — Reasoning
  crow-brain       Intent compiler, budget governor, dual-track MCTS

Layer 5 — Interface & Observability
  crow-cli         Ratatui TUI (the user-facing binary)
  crow-replay      Behavioral regression & task replay harness
```

### Dependency Convention

Crates may only depend on crates in **equal or lower** layers. This is
enforced by code review today; a `cargo-deny` or `xtask lint-deps` policy
is planned. Circular dependencies are caught by `cargo check`.

## Build & Test Commands

```bash
# Check compilation (all crates)
cargo check --workspace

# Run all tests
cargo test --workspace

# Run tests for a single crate
cargo test -p crow-patch

# Build the CLI binary
cargo build -p crow-cli
```

## Code Conventions

- **No external dependencies in Currency crates** (Layer 0). They use only `std`.
- **Every public type must derive** at minimum: `Debug, Clone, PartialEq, Eq`.
- **Ordered enums** (like `Confidence`, `LanguageTier`) must also derive `PartialOrd, Ord`.
- **Tests live in `#[cfg(test)] mod tests`** inside each types module. No separate test files until integration tests are needed.
- **Workspace-relative paths** are represented as `WorkspacePath` (validated newtype), never bare `PathBuf`.
- **No `unwrap()` in library code.** Use `Result` or `Option` propagation.

## System Invariants (from RFC-001)

1. The LLM **never writes to disk directly.** All mutations go through `IntentPlan`.
2. Final disk flushes **must verify** `base_snapshot_id` preconditions.
3. A failed operation **must leave the workspace untouched** (zero pollution).
4. Every risk flag or test result **must trace back** to a concrete command log or snapshot.

## Current Status

- **Step 1** ✅ Workspace genesis — 10 crates, `cargo check` green.
- **Step 2** ✅ Core data contracts — `crow-patch` (12 tests), `crow-evidence` (10 tests), `crow-probe` (7 tests).
- **Step 3** 🔄 `crow-materialize` — OS-level sandbox (APFS clonefile, symlink, fallback).
- **Step 4** 🔲 `crow-verifier` — ACI log truncation.
- **Step 5** 🔲 `crow-probe` — Detection heuristics implementation.
