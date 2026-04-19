# 🦅 Crow Code (The Interactive Developer Workstation)

> **Evidence-driven AI coding agent with sandboxed verification, non-blocking sub-agent swarms, and structured patch plans.**

**Crow Code** is an AI coding agent built by [CorvusMatrix](https://github.com/CorvusMatrix). Instead of letting a model write directly to your repository, Crow compiles model output into structured `AgentAction` / `IntentPlan` objects, rehydrates them against the current workspace, applies them inside an isolated sandbox, and verifies the result before any workspace write.

With the advent of **Console 6.0**, Crow is a **world-class interactive High-Performance TUI Workstation** featuring dynamic skill loading, autonomous context compaction, and non-blocking swarm concurrency.

## ✨ Why Crow Code?

### **1. 🚀 Codex-Style Interactive TUI & Swarm Concurrency**
- **Non-Blocking Execution**: Type freely while the agent is "thinking". Your inputs are asynchronously queued and cleanly burst-executed the moment the turn completes.
- **Sub-Agent Swarms (`/swarm`)**: Delegate localized research, file generation, or massive refactoring tasks to detached, concurrent agents. Track them via the dynamic **Swarm Activity Bar** without losing focus on your main interactive thread.
- **Session Persistence**: Crow seamlessly checkpoints conversations to `~/.crow/sessions/`. Start it back up with `crow -r` and the entire cognitive context is rehydrated instantly.

### **2. 🛡️ Evidence Over Vibes (Sandboxed Verification)**
- **Patches are First-Class**: The model proposes structured actions; it does not write arbitrary text straight to disk.
- **Snapshot-Aware Safety**: Plans are anchored to a workspace snapshot and hydrated with ground-truth file hashes *before* applying.
- **Isolated Sandboxing**: Changes are completely materialized inside an APFS/Hardlink isolated sandbox. Crow executes ACI-restricted validation pre-flights on your test suite. Only victorious patches are merged into the live repository.

### **3. 🧠 Rich Context & Extensibility**
- **Dynamic Skill System**: Drop `SKILL.md` files into `~/.crow/skills/` or `.crow/skills/` with YAML frontmatter to inject project-specific prompts and behaviors. Skills are advertised to the LLM via lightweight XML metadata tags — no token waste.
- **Autonomous Context Compactor**: 2-phase compaction (micro-compact tool results → full LLM summarization) automatically maintains context health during long sessions without manual intervention.
- **Multi-Provider Routing**: Out-of-the-box compatibility with OpenAI, Anthropic, Ollama, and DeepSeek backends.
- **MCP Stdio Transport**: Extensible Model Context Protocol (MCP) tooling natively baked into the event bus.
- **High-Granularity Event System**: Real-time token usage tracking, agent state transitions, retry visualization, and compaction indicators streamed to the TUI.

---

## 🚀 Getting Started

### 1️⃣ Installation

Currently, Crow is built from source. You need **Rust and Cargo** installed on your system.

```bash
# 1. Clone the repository
git clone https://github.com/CorvusMatrix/crow-code.git
cd crow-code

# 2. Build and install the binary globally
cargo install --path crates/crow-cli

# 3. Verify installation
crow --help
```
*Note: Ensure `~/.cargo/bin` is in your system `$PATH`.*

### 2️⃣ Configuration

Your workspace needs access to an LLM provider. Crow supports multiple backends. You can configure this via **Environment Variables** for quick tests, or via a **JSON Config File** for permanent settings.

#### Option A: Quick Environment Variables
```bash
# OpenAI
export OPENAI_API_KEY="sk-..."
export LLM_PROVIDER="openai"
export LLM_MODEL="gpt-4o"

# Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."
export LLM_PROVIDER="anthropic"
export LLM_MODEL="claude-sonnet-4-20250514"
```

#### Option B: Global or Local Configuration
Crow looks for configuration in two places:
1. **Global**: `~/.crow/config.json` (Applies everywhere)
2. **Local**: `.crow/config.json` (Override settings per-project)

Example `~/.crow/config.json` using **Ollama** (Local AI):
```json
{
  "llm": {
    "provider": "ollama",
    "model": "llama3.1:8b",
    "base_url": "http://localhost:11434/v1"
  },
  "workspace": {
    "write_mode": "write", 
    "map_budget": 65536
  }
}
```
*Tip: `write_mode: "write"` allows Crow to apply verified patches to your repo. Set it to `"sandbox_only"` to preview changes without modifying your files.*

### 3️⃣ Skills (Dynamic Prompt Plugins)

Create project-specific or global skills by adding `SKILL.md` files:

```bash
# Global skills
mkdir -p ~/.crow/skills/rust-expert
cat > ~/.crow/skills/rust-expert/SKILL.md << 'EOF'
---
description: Expert-level Rust coding guidance
triggers:
  - rust
  - cargo
---
# Rust Expert Skill
Prefer idiomatic Rust patterns. Use `thiserror` for errors...
EOF

# Project-local skills
mkdir -p .crow/skills/
```

Skills are automatically discovered and advertised to the LLM via `<skill>` XML tags. The body content is loaded on-demand, not injected into every API call.

### 4️⃣ Usage (The Interactive Workstation)

Navigate to any codebase you want to work on and start the Crow TUI:

```bash
cd my-project
crow
```

You are now in the Interactive Developer Workstation. 
- **Type naturally** to instruct the agent to build features or fix bugs. 
- **Asynchronous Flow**: While Crow is "thinking", you don't have to wait! Continue typing commands, and they will be queued seamlessly.
- **Sub-Agent Swarms**: Need a massive refactor done in the background? Type `/swarm audit all error handling`. A detached agent will take off on your Swarm Bar.

**Instant Resume**: Had to close the terminal? Run `crow -r` to instantly rehydrate your entire active session!

---

## 💻 Core Command Reference

| Pipeline Command | Purpose |
|---|---|
| `crow` | Open the Interactive Ratatui Workbench. |
| `crow -r` \| `--resume` | Boot the Workbench, resuming the most recently active session. |
| `crow chat` | Start a simple REPL chat mode (no TUI). |
| `crow compile <prompt>` | Output the structured `AgentAction` JSON. |
| `crow plan <prompt>` | Fast evidence-first preview of plans and impact reports. |
| `crow run <prompt>` | The full autonomous sandbox pipeline (Agentic Loop). |
| `crow dry-run <prompt>` | Alias for `run`. |
| `crow dashboard` | Open the interactive EventLedger & Dream dashboard. |
| `crow dream` | Run background AutoDream memory consolidation. |
| `crow session list` | View all historical JSONL sessions. |
| `crow session resume <id>`| Resume a specific session timeline. |
| `crow mcp` | Manage MCP tools. |

### Workbench Slash Commands
When inside the TUI, these commands orchestrate the session:

- `/help` — Display the integrated help manual.
- `/swarm <task>` — Launch a detached, truly concurrent background agent process.
- `/model <name>` — Live-swap the active LLM router strategy.
- `/clear` — Erase current semantic memory buffers.
- `/status` — Dump advanced diagnostic engine state to history.
- `/view <mode>` — Switch lens mode (`focus` | `evidence` | `audit`).

### Environment Variables

| Variable | Purpose |
|---|---|
| `OPENAI_API_KEY` | API key (or `CROW_API_KEY`) |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `LLM_BASE_URL` | Provider endpoint |
| `LLM_MODEL` | Model name |
| `LLM_PROVIDER` | Provider type (`openai`, `anthropic`, `ollama`, `custom`) |
| `CROW_WRITE_MODE` | `sandbox` \| `write` \| `danger` (default: `write`) |
| `CROW_MCTS_BRANCHES` | MCTS branch factor (default: `3`) |
| `CROW_MAP_BUDGET` | Repo map size budget in bytes |

---

## 🧱 Architecture

Crow relies on a strictly layered, multi-crate topology to shield core execution boundaries:

```
L5  crow-cli   crow-replay   crow-mcp
L4  crow-brain
L3  crow-intel
L2  crow-verifier
L1  crow-workspace   crow-materialize
L0  crow-patch   crow-evidence   crow-probe
```

| Crate | Layer | Purpose |
|-------|-------|---------| 
| `crow-patch` | L0 | Internal patch contract: `AgentAction`, `IntentPlan`, `WorkspacePath` |
| `crow-evidence` | L0 | Verification evidence schemas & multidimensional types |
| `crow-probe` | L0 | Active workspace scanning and validation candidates |
| `crow-workspace` | L1 | Plan hydration, mutations applier, and Ledger event ingestion |
| `crow-materialize`| L1 | Secure Physical Isolation protocols (CoW / symlink boundaries) |
| `crow-verifier` | L2 | Sandboxed command execution & ACI truncation buffers |
| `crow-intel` | L3 | LSP bridges, Tree-Sitter Repo Maps & outliners |
| `crow-brain` | L4 | Intent compilation, LLM streaming, Skill system, Context compactor |
| `crow-cli` | L5 | The Event-Bus UX, Ratatui TUI, MCTS crucible & Swarm managers |
| `crow-mcp` | L5 | MCP stdio JSON-RPC dueling clients |

---

## 🗺️ Current Status

### ✅ **Achieved & Deployed**
- Multi-provider LLM routing with streaming tool-calls.
- Complete snapshot anchoring & rollback runtime validation.
- Sub-Agent delegation constraints with deep recursion checks.
- Unified ThreadManager yielding a completely asynchronous, dynamic TUI experience.
- Queue-based input buffering & zero-block Multi-Task Swarms.
- Per-tool Security Wall approval loops (Whitelist overrides).
- **Dynamic Skill System** with YAML frontmatter and XML prompt injection.
- **2-Phase Context Compactor** (micro-compact → full LLM summarization).
- **High-granularity Event System** with TokenUsage, Retrying, Compacting, StateChanged events.
- **Elm/Redux TUI Components** (ChatView, CommandPalette, InfoBar with token gauge).

### 🚧 **Active Development**
- **Agent State Machine**: Explicit state transitions (Idle → Streaming → ExecutingTool → WaitingForInput).
- **Incremental Markdown Streaming**: Real-time rendering of code blocks and headings as they stream in.
- **Time-Travel Replay**: The `crow-replay` harness for behavioral regression.
- **Deep LSP Intelligence**: Tighter native LSP proxy streams through `crow-intel`.

For in-depth architectural mandates, check out [`docs/RFC-001-Architecture-Baseline.md`](docs/RFC-001-Architecture-Baseline.md).

---

## 📜 License
[MIT](LICENSE)