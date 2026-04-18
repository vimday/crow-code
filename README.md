# 🦅 Crow Code

![Corvus Matrix Logo](./assets/logo.png)

> The smartest bird in the codebase.

**Crow Code** is a next-generation AI coding agent built by [CorvusMatrix](https://github.com/CorvusMatrix). It is being designed from the ground up with evidence-driven verification, zero-pollution guarantees, and time-travel safety — no legacy, no compromises.

## Why Crow?

Crows are the most intelligent birds on the planet: they use tools, plan ahead, and solve multi-step problems. This project embodies that philosophy:

- **Evidence over confidence scores.** Every proposed change will carry a structured proof bundle — compile results, test run history, lint status, and semantic risk flags — not an opaque 0–100 number.
- **Patches are first-class citizens.** The LLM never touches your files directly. All mutations are buffered as atomic `IntentPlan`s anchored to workspace snapshots.
- **Zero pollution guarantee (design goal).** If anything fails, your workspace stays exactly as it was. This is the invariant the runtime is being built to enforce.
- **Time-travel (planned).** Event-sourced state machine with O(1) snapshots. Undo, redo, branch — at the infrastructure level.

## Architecture

```
┌──────────────────────────────────────────────────┐
│  crow-cli  crow-replay  crow-mcp                │  L5: Interface
├──────────────────────────────────────────────────┤
│  crow-brain  (Intent Compiler + MCTS Solver)     │  L4: Reasoning
├──────────────────────────────────────────────────┤
│  crow-intel  (Tree-sitter + LSP Bridge)          │  L3: Intelligence
├──────────────────────────────────────────────────┤
│  crow-verifier  (Workspace Exec + ACI Truncation)│  L2: Crucible
├──────────────────────────────────────────────────┤
│  crow-workspace        crow-materialize          │  L1: Runtime
│  (Hydrator & Applier)   (Workspace Copy / CoW)   │
├──────────────────────────────────────────────────┤
│  crow-patch    crow-evidence    crow-probe       │  L0: Currencies
│  (Patch Contract) (Evidence Matrix) (Recon Radar)│
└──────────────────────────────────────────────────┘
```

11 Rust crates, strict layered dependencies, zero external deps at the foundation.

See [`docs/RFC-001-Architecture-Baseline.md`](docs/RFC-001-Architecture-Baseline.md) for the full design document.

**Note on current status:** Core data contracts, sandbox materialization, preflight verification, MCTS iteration, session persistence, snapshot anchoring, multi-provider routing, and MCP stdio transport are implemented. The Event Ledger and parts of the Replay Harness remain planned or partial.

## Quick Start

```bash
# Build
cargo build --workspace

# Run all tests
cargo test --workspace

# Run the CLI
cargo run -p crow-cli
```

## Project Status

| Step | Description | Status |
|------|-------------|--------|
| 1 | Workspace genesis | ✅ |
| 2 | Core data contracts (`crow-patch`, `crow-evidence`, `crow-probe`) | ✅ |
| 3 | `crow-materialize` sandbox isolation | ✅ |
| 4 | `crow-verifier` execution + ACI truncation | ✅ |
| 5 | Probe scanner, workspace applier, CLI pipeline | ✅ |
| 6 | MCTS parallel crucible & cache isolation | ✅ |
| 7 | Polyglot preflights & snapshot verification | ✅ |
| 8 | Session persistence, evidence report, CLI subcommands | ✅ |
| 9 | Real snapshot anchoring & workspace write enforcement | ✅ |
| 10 | Multi-provider LLM routing | ✅ |
| 11 | MCP stdio transport (`crow-mcp`) | ✅ |

## Crate Overview

| Crate | Layer | Purpose |
|-------|-------|---------|
| `crow-patch` | L0 | Unified patch contract: `EditOp`, `IntentPlan`, `WorkspacePath` |
| `crow-evidence` | L0 | Multidimensional verification: `EvidenceMatrix`, `TestRun`, `RiskFlag` |
| `crow-probe` | L0 | Repository radar: `ProjectProfile`, `VerificationCandidate` |
| `crow-workspace` | L1 | Plan hydration and sandbox mutation applier |
| `crow-materialize` | L1 | Workspace-isolation CoW/copy materialization |
| `crow-verifier` | L2 | Workspace-isolated command execution, log truncation |
| `crow-intel` | L3 | Tree-sitter outlines, LSP bridge |
| `crow-brain` | L4 | Intent compiler, budget governor, MCTS |
| `crow-cli` | L5 | CLI entrypoint — the `crow` binary |
| `crow-replay` | L5 | Behavioral regression and task replay harness (planned) |
| `crow-mcp` | L5 | JSON-RPC 2.0 stdio MCP transport and client wrapper |

## License

[MIT](LICENSE)