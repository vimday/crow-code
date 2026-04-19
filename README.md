<p align="center"><code>cargo install --path crates/crow-cli</code><br />or <code>crow --help</code></p>
<p align="center"><strong>Crow CLI</strong> is a coding agent from CorvusMatrix that runs locally on your computer.</p>
<br/>

If you want an open-source, evidence-driven autonomous coding developer workstation with a dynamic ratatui-powered TUI, non-blocking agentic swarms, and pure sandboxed verifications, you are in the right place.

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

## License

This repository is licensed under the [MIT License](LICENSE).