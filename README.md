# 🦅 Crow Code (The Interactive Developer Workstation)

> **Evidence-driven AI coding agent with sandboxed verification, non-blocking sub-agent swarms, and structured patch plans.**

**Crow Code** is an AI coding agent built by [CorvusMatrix](https://github.com/CorvusMatrix). Instead of letting a model write directly to your repository, Crow compiles model output into structured `AgentAction` / `IntentPlan` objects, rehydrates them against the current workspace, applies them inside an isolated sandbox, and verifies the result before any workspace write.

With the advent of **Phase 3**, Crow is no longer just a pipeline—it is a **world-class interactive High-Performance TUI Workstation**. You can chat, queue commands seamlessly, and instantly dispatch parallel background sub-agents while the primary core continues working.

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
- **Native Tool Calling**: Tightly integrated into streaming pipelines.
- **Multi-Provider Routing**: Out-of-the-box compatibility with OpenAI, Anthropic, Ollama, and DeepSeek backends.
- **MCP Stdio Transport**: Extensible Model Context Protocol (MCP) tooling natively baked into the event bus.

---

## ⚡ Quick Start

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
export LLM_MODEL="claude-3-5-sonnet-20240620"
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

### 3️⃣ Usage (The Interactive Workstation)

Navigate to any Rust codebase you want to work on and start the Crow TUI:

```bash
cd my-rust-project
crow
```

You are now in the Interactive Developer Workstation. 
- **Type naturally** to instruct the agent to build features or fix bugs. 
- **Asynchronous Flow**: While Crow is "thinking", you don't have to wait! You can continue typing commands, and they will be queued seamlessly.
- **Sub-Agent Swarms**: Need a massive refactor done in the background? Type `/swarm audit all error handling`. A detached agent will take off on your Swarm Bar, leaving your main terminal free for continuing work.

**Instant Resume**: Had to close the terminal? Run `crow -r` to instantly rehydrate your entire active session, context history, and verification records!

---

## 💻 Core Command Reference

| Pipeline Command | Purpose |
|---|---|
| `crow` | Open the Interactive Ratatui Workbench. |
| `crow -r` \| `--resume` | Boot the Workbench, resuming the most recently active session. |
| `crow compile <prompt>` | Output the structured `AgentAction` JSON. |
| `crow plan <prompt>` | Fast evidence-first preview of plans and impact reports. |
| `crow run <prompt>` | The full autonomous sandbox pipeline (Agentic Loop). |
| `crow dry-run <prompt>` | Alias for `run`. |
| `crow session list` | View all historical JSONL sessions. |
| `crow session resume <id>`| Resume a specific session timeline. |

### Workbench Slash Commands
When inside the TUI, these commands orchestrate the session:

- `/help` — Display the integrated help manual.
- `/swarm <task>` — Launch a detached, truly concurrent background agent process on a secondary task.
- `/model <name>` — Live-swap the active LLM router strategy safely.
- `/clear` — Erase current semantic memory buffers.
- `/status` — Dump advanced diagnostic engine state to history.

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
| `crow-brain` | L4 | Intent compilation, LLM client streaming, & AutoDream models |
| `crow-cli` | L5 | The Event-Bus UX, Ratatui GUI, MCTS crucible & Swarm thread managers |
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

### 🚧 **Active Development**
- **Time-Travel Replay**: The `crow-replay` harness for behavioral regression is in active design.
- **Event-Ledger UX**: Enhanced visualization of timeline snapshots onto the Dashboard.
- **Deep LSP Intelligence**: Tighter native LSP proxy streams through `crow-intel`.
- **Advanced Network Isolation**: Broadening OS-level process sandboxing constraints.

For in-depth architectural mandates, check out [`docs/RFC-001-Architecture-Baseline.md`](docs/RFC-001-Architecture-Baseline.md).

---

## 📜 License
[MIT](LICENSE)