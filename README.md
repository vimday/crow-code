<p align="center"><img src="assets/logo.png" width="120" alt="Crow Logo" /></p>
<h3 align="center">Crow CLI</h3>
<p align="center">An evidence-driven AI coding agent that runs locally.<br/>
Built with Rust. Verified in sandboxes. No cloud dependency.</p>
<p align="center">
<code>cargo install --path crates/crow-cli</code>
</p>

---

## What Crow Does Today

Crow is a terminal-based AI coding agent that generates, verifies, and applies code changes to your workspace. It differs from typical AI coding tools in one key way: **every proposed change is sandbox-verified before touching your code**.

**Production-ready capabilities:**
- **Interactive TUI Workbench** — A ratatui-powered terminal UI with streaming markdown, session persistence, and multi-line editing.
- **Monte Carlo Tree Search (MCTS) Crucible** — Parallel exploration and verification of code patches against immutable workspace snapshots.
- **Epistemic Loop** — An autonomous reasoning cycle with structured tool use (file reads, shell commands, grep, subagent delegation).
- **Multi-Provider LLM Support** — OpenAI-compatible and Anthropic APIs, with automatic error retry and exponential backoff.
- **MCP Integration** — Stdio-transport MCP server support for extending the tool registry.
- **Session Persistence** — Save and resume coding sessions across restarts with `crow -r`.
- **Context Compaction** — Automatic conversation pruning when approaching token limits, preserving role alternation invariants.

**Not yet production-ready:**
- AutoDream memory consolidation (experimental)
- Replay harness for behavioral regression testing (stub only)

---

## Quick Start

### Prerequisites

- **Rust** ≥ 1.75 (with `cargo`)
- **macOS** or **Linux** (Windows untested)
- An LLM API key (OpenAI, Anthropic, or compatible)
- `git` (for snapshot anchoring and branch detection)

### Install

```shell
git clone https://github.com/CorvusMatrix/crow-code.git
cd crow-code
cargo install --path crates/crow-cli
```

### Configure your LLM provider

Set environment variables or create `.crow/config.json` in your workspace:

```shell
# OpenAI (default provider)
export OPENAI_API_KEY="sk-..."
export LLM_MODEL="gpt-4o"

# Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."
export LLM_PROVIDER="anthropic"
export LLM_MODEL="claude-sonnet-4-20250514"

# Custom OpenAI-compatible endpoint
export LLM_BASE_URL="https://your-endpoint.com/v1"
export LLM_PROVIDER="openai"
```

### Run

```shell
# Open the interactive workbench
crow

# Resume the last session
crow -r

# Run a single autonomous task
crow run "add error handling to the parse module"

# Preview a plan without applying
crow plan "refactor the auth middleware"
```

After launching `crow`, type a natural language task and press Enter. The agent will analyze your codebase, generate a plan, verify it in a sandbox, and apply the changes.

---

## Commands

| Command | Description |
|---|---|
| `crow` | Open the interactive TUI workbench |
| `crow -r` \| `--resume` | Resume the most recent session |
| `crow run <prompt>` | Run the full autonomous pipeline with MCTS Verification |
| `crow yolo <prompt>` | Run the fast-path native tool-calling mode (Codex style) |
| `crow plan <prompt>` | Preview a verified plan without applying |
| `crow compile <prompt>` | Output raw IntentPlan JSON |
| `crow session list` | List saved sessions |
| `crow session resume <id>` | Resume a specific session |
| `crow dream` | Run AutoDream memory consolidation (experimental) |
| `crow mcp list-tools` | List available MCP tools |

### TUI Shortcuts

| Key | Action |
|---|---|
| `Enter` | Submit prompt |
| `Ctrl+J` | Insert newline |
| `Ctrl+C` | Interrupt task / press twice to quit |
| `Ctrl+D` | Quit immediately |
| `Esc` | Interrupt running task |
| `Tab` | Switch focus (composer ↔ history) |
| `PageUp/Down` | Scroll history |
| `/help` | Show all slash commands |
| `!<cmd>` | Execute shell command (with approval dialog) |

---

## Configuration

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `OPENAI_API_KEY` | — | OpenAI API key (or `CROW_API_KEY`) |
| `ANTHROPIC_API_KEY` | — | Anthropic API key |
| `LLM_PROVIDER` | `openai` | Provider type (`openai`, `anthropic`, `custom`) |
| `LLM_MODEL` | `gpt-4o` | Model identifier |
| `LLM_BASE_URL` | — | Custom endpoint URL |
| `CROW_WRITE_MODE` | `write` | `sandbox` (verify only), `write` (apply after verify), `danger` (skip verify) |
| `CROW_MCTS_BRANCHES` | `3` | MCTS parallel branch factor |
| `CROW_MAP_BUDGET` | — | Repo map size budget (bytes) |

### Safety Model

Crow defaults to `write` mode. All mutations go through this pipeline:

1. **Snapshot** — Freeze the workspace via `git rev-parse HEAD` (3-tier fallback).
2. **Materialize** — Create an isolated sandbox copy (APFS clonefile on macOS, safe copy fallback).
3. **Verify** — Run the plan in the sandbox with build/test/lint checks.
4. **Apply** — Only if verification passes, apply changes to the real workspace.

In `sandbox` mode, step 4 is skipped. In `danger` mode, step 3 is skipped.

A failed operation **never modifies your workspace** (zero-pollution guarantee).

---

## Architecture

```
┌────────────────────────────────────────────────┐
│  TUI Layer (ratatui)                           │
│  ┌──────────┬──────────┬──────────┬──────────┐ │
│  │ History  │ Composer │ InfoBar  │ Cmd      │ │
│  │ Component│ Component│ (tokens, │ Palette  │ │
│  │          │ (focus)  │ ctx %)   │          │ │
│  └──────────┴──────────┴──────────┴──────────┘ │
│  StreamController (tick-based line draining)    │
│  StreamingMarkdownRenderer (newline-gated)      │
├────────────────────────────────────────────────┤
│  ThreadManager ←→ ConversationManager          │
│  ┌──────────────────────────────────────────┐  │
│  │ SessionRuntime                           │  │
│  │  → Materialization (frozen snapshot)     │  │
│  │  → Epistemic Loop (ReconAction cycle)    │  │
│  │  → Crucible (MCTS verification)          │  │
│  │  → Subagent Workers (bounded 120s)       │  │
│  └──────────────────────────────────────────┘  │
├────────────────────────────────────────────────┤
│  crow-brain  │ crow-patch │ crow-verifier      │
│  (LLM client)│ (IntentPlan│ (sandbox exec)     │
│              │  EditOps)  │                    │
└────────────────────────────────────────────────┘
```

### Crate Topology

| Layer | Crate | Role |
|---|---|---|
| L0 — Currencies | `crow-patch` | Unified patch contracts (EditOp, IntentPlan, Confidence) |
| L0 | `crow-evidence` | Multidimensional verification evidence (EvidenceMatrix) |
| L0 | `crow-probe` | Repository recon (ProjectProfile, language detection) |
| L1 — Runtime | `crow-workspace` | Plan hydration & sandbox mutation applier |
| L1 | `crow-materialize` | Workspace-isolation via CoW / safe-copy |
| L2 — Crucible | `crow-verifier` | Isolated command execution & ACI log truncation |
| L3 — Intelligence | `crow-intel` | Tree-sitter outlines, LSP bridge |
| L4 — Reasoning | `crow-brain` | Intent compiler, budget governor, dual-track MCTS |
| L5 — Interface | `crow-cli` | Ratatui TUI (the user-facing binary) |
| L5 | `crow-mcp` | MCP stdio transport (JSON-RPC 2.0) |

Crates may only depend on crates in equal or lower layers. `cargo check` catches circular violations.

### Key Design Patterns

- **Snapshot Safety** — The workspace is frozen before planning. The epistemic loop and crucible both operate against the same immutable snapshot.
- **Role Alternation** — `ConversationManager::fix_role_alternation()` ensures strict User→Assistant→User ordering after compaction.
- **Hallucination Guard** — The epistemic loop rejects `Modify` operations on files the agent hasn't read during the current turn.
- **Delegation Depth Limiting** — Subagent delegation is capped at 3 levels to prevent infinite recursion.

---

## Documentation

- [**Agent & Developer Guidelines**](./AGENTS.md) — Coding conventions, TUI styling rules, and architectural invariants.
- [**Architecture RFC**](./docs/RFC-001-Architecture-Baseline.md) — The original design constitution.

## License

This repository is licensed under the [MIT License](LICENSE).