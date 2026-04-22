<p align="center"><code>cargo install --path crates/crow-cli</code><br />or <code>crow --help</code></p>
<p align="center"><strong>Crow CLI</strong> is a coding agent from CorvusMatrix that runs locally on your computer.</p>
<br/>

If you want an open-source, evidence-driven autonomous coding developer workstation with a dynamic ratatui-powered TUI, non-blocking agentic swarms, and pure sandboxed verifications, you are in the right place.

## Core Capabilities
- **Monte Carlo Tree Search (MCTS) Crucible**: Parallel execution and validation of generative patching against an immutable snapshot representation of your code.
- **Micro-Compaction Memory Pipeline**: Retains cognitive coherence over long sessions by asynchronously rewriting and pruning older conversation history to strictly abide by tokenizer limits.
- **Implicit Skill Resolution**: Dynamically ingests `SKILL.md` configurations scoped to your workspace and resolves matching capabilities intelligently against unstructured prompts.
- **AutoDream Consolidation**: Spawns isolated long-term structural processing daemons to compress temporal execution traces into rigid architectural invariants for downstream sessions.
- **Structured Turn Lifecycle**: Every agent turn emits `TurnEvent` variants (`Started`, `PhaseChanged`, `Completed`, `Aborted`) enabling deterministic UI state tracking and telemetry.
- **Newline-Gated Markdown Streaming**: Streaming markdown is rendered incrementally using a committed-lines cache (ported from Codex's `MarkdownStreamCollector`), reducing rendering overhead from O(n²) to O(n).
- **Error Categorization & Retry**: `BrainError` classifies errors as transient (HTTP 429/500/502/503/529) or permanent (401/403, parse errors), with exponential backoff retry for transient failures.

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

### Key Patterns
- **Snapshot Safety**: The workspace is frozen before planning. The epistemic loop and crucible both operate against the same immutable snapshot, preventing time-of-check/time-of-use divergence.
- **Role Alternation**: `ConversationManager::fix_role_alternation()` ensures strict User→Assistant→User ordering after compaction, required by Anthropic-style providers.
- **Hallucination Guard**: The epistemic loop rejects `Modify` operations on files the agent hasn't read during the current turn.
- **Delegation Depth Limiting**: Subagent delegation is capped at 3 levels to prevent infinite recursion.

---

## Quickstart

### Installing and running Crow CLI

Currently, Crow is built directly from source. You need **Rust and Cargo** installed on your system.

```shell
# Clone the repository
git clone https://github.com/CorvusMatrix/crow-code.git
cd crow-code

# Install using Cargo
cargo install --path crates/crow-cli
```

Then simply navigate to any workspace and run `crow` to open the Interactive Developer Workstation.

### Connecting your LLM provider

Crow requires an LLM to generate actions. You can configure this via **Environment Variables** or `.crow/config.json`.

```shell
# OpenAI (Default)
export OPENAI_API_KEY="sk-..."
export LLM_PROVIDER="openai"
export LLM_MODEL="gpt-4o"

# Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."
export LLM_PROVIDER="anthropic"
```

## Docs

- [**Agent & Developer Guidelines**](./AGENTS.md)
- [**Architecture RFC Baseline**](./docs/RFC-001-Architecture-Baseline.md)

## Core Commands

| Command | Purpose |
|---|---|
| `crow` | Open the Interactive Ratatui Workbench. |
| `crow -r` \| `--resume` | Boot the Workbench, resuming the most recently active session. |
| `crow run <prompt>` | Run the autonomous sandbox pipeline (Agentic Loop) statelessly. |
| `crow plan <prompt>` | Fast evidence-first preview of MCTS generated plans. |

When inside the TUI, orchestrate the session effortlessly using `/help`, `/swarm <task>`, `/clear`, and `/model <model>`.

## TUI Components

| Component | Description |
|---|---|
| **HistoryComponent** | Scrollable conversation pane with semantic cell rendering (user messages, agent markdown, tool results). |
| **ComposerComponent** | Multi-line text input with cursor tracking, focus management, and explicit `set_cursor()` for terminal visibility. |
| **InfoBar** | Streaming-aware status bar: model name, git branch, token gauge with color-coded context window usage. |
| **CommandPalette** | Dynamic slash-command popup (`/help`, `/clear`, `/model`, `/swarm`). |
| **StreamController** | Tick-based line draining (1 line/120ms, batch 5/tick when backlog > 20) to prevent UI stuttering during streaming. |

## License

This repository is licensed under the [MIT License](LICENSE).