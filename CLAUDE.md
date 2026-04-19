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
Layer 0 — Currencies (std + serde; no runtime deps)
  crow-patch       Unified patch contract (EditOp, IntentPlan, Confidence)
  crow-evidence    Multidimensional verification evidence (EvidenceMatrix)
                   Re-exports crow-patch::Confidence as canonical type.
  crow-probe       Repository recon radar (ProjectProfile)

Layer 1 — Runtime
  crow-workspace   Plan hydration & sandbox mutation applier (Event Ledger planned)
  crow-materialize Workspace-isolation materialization (CoW / copy)

Layer 2 — Crucible
  crow-verifier    Workspace-isolated command execution & ACI log truncation

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

- **Currency crates (L0) use only `std` + `serde` + `schemars`.** No runtime dependencies (HTTP, async, filesystem-heavy crates).
- **Every public type must derive** at minimum: `Debug, Clone, PartialEq, Eq`.
- **Ordered enums** (like `Confidence`, `LanguageTier`) must also derive `PartialOrd, Ord`.
- **Tests live in `#[cfg(test)] mod tests`** inside each types module. No separate test files until integration tests are needed.
- **Workspace-relative paths** are represented as `WorkspacePath` (validated newtype), never bare `PathBuf`.
- **No `unwrap()` in library code.** Use `Result` or `Option` propagation.

## System Invariants (from RFC-001)

1. The LLM **never writes to disk directly.** All mutations go through `IntentPlan`.
2. Final disk flushes **must verify** `base_snapshot_id` preconditions.
3. A failed operation **must leave the workspace untouched** (zero pollution).
4. Every risk flag or test result **must trace back** to a concrete command log or snapshot. (Ledger planned)

## Current Status

- **Step 1** ✅ Workspace genesis — 10 crates, `cargo check` green.
- **Step 2** ✅ Core data contracts — `crow-patch` (24 tests), `crow-evidence` (10 tests), `crow-probe` (12 tests).
- **Step 3** ✅ `crow-materialize` — Workspace-isolation physical materialization (21 tests). APFS clonefile, SafeCopy, HardlinkTree (opt-in only). Symlink boundary enforcement, SandboxGuard RAII.
- **Step 4** ✅ `crow-verifier` — Workspace-isolated execution + ACI log truncation (20 tests). Direct exec (no shell), head+tail truncation, VerificationResult → EvidenceMatrix.
- **Step 5** ✅ `crow-probe` scanner, `crow-workspace` applier, `crow-cli` God Pipeline.
- **Step 6** ✅ MCTS Parallel Crucible & Cache Isolation. Epistemic loop, preflight compile checks, ConversationManager, build cache warm-up, early termination.
- **Step 7** ✅ Polyglot Preflights & Snapshot Anchor Runtime Verification. Manifest-aware walker.
- **Step 8** ✅ Phase 1 Product Foundation: Session persistence (`~/.crow/sessions/`), `crow -r` Instant Rehydration, Evidence Report module, CLI subcommands.
- **Step 9** ✅ Phase 1.5 Promise Closures: Real snapshot anchoring via `git rev-parse HEAD` (3-tier fallback), WriteMode runtime enforcement in execute path with `apply_sandbox_to_workspace()`.
- **Step 10** ✅ Phase 2 Multi-Provider: `AnthropicClient`, `ProviderRouter`, `LLM_PROVIDER` overrides.
- **Step 11** ✅ Phase 2 MCP Stdio Transport: New `crow-mcp` crate. JSON-RPC 2.0 full-duplex protocol over `tokio::process::Command` stdio. Ergonomic `McpClient` wrapper.
- **Step 12** ✅ Architectural Fusion (Phase 1 & 2): Integrated `Codex` native TUI mechanics, `ThreadManager` non-blocking agent loop with TurnStatus isolation, `yomi` inspired Context Persistence & per-tool Memory Whitelist (`A` auto-approve security wall).
