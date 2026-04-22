# Developer & Agent Guidelines

Welcome to the `crow-code` repository. This document outlines the architectural boundaries, coding styles, and workflow conventions for all developers and autonomous agents contributing to this project. 

These rules draw heavily from professional agentic environments (inspired by Codex) to keep this repository clean, predictable, and robust.

---

## 1. Safety and Execution Boundaries
- **No Direct Shell Overrides**: Never use root-level or out-of-bounds shell commands without the user's explicit consent.
- **MCTS Stability**: The `crow-brain` MCTS engine has explicit parallelism timeouts. Do not remove or bypass the `180s` global exploration limit or the `120s` branch-level `tokio::time::timeout` limits. These were added to prevent deadlocks and network-induced hanging.
- **Subagent Timeout**: All subagent workers are wrapped in a `120s` tokio timeout (`subagent.rs`). Do not remove this or extend it without justification.
- **Build Isolation**: Avoid polluting the host workspace when testing patches. If executing `diff` or `git patch` internally, ensure they strictly run against the sandbox environment first.
- **Shell Security**: The TUI shell escape (`!command`) uses a metacharacter blocklist to prevent injection. When adding new shell features, always route through the approval dialog for unrecognized patterns. The blocklist includes `$(`, `${`, backticks, pipes, redirects, and comment characters.
- **Panic Safety**: The TUI installs a panic hook that restores terminal state (raw mode, alternate screen) before any panic propagates. Do not remove or bypass this hook.

## 2. Rust Conventions & Lints
We use a highly aggressive `[workspace.lints.clippy]` block inherited across all crates. 
If `cargo check` fails on Lints, fix them rather than ignoring them.

- **`uninlined_format_args`**: Always inline format arguments (`format!("hello {world}")` instead of `format!("hello {}", world)`).
- **`collapsible_if`**: Collapse nested `if/else` statements.
- **`redundant_closure_for_method_calls`**: Pass methods directly where applicable.
- **`unwrap_used`**: The workspace currently warns on `.unwrap()`. In core engine modules (`crow-brain`, `crow-patch`, `crow-verifier`), handle errors natively returning `Result::Err`. Only use `unwrap()` defensively inside TUI layout operations if error boundaries are truly infallible.
- **`too_many_arguments`**: Warns on functions with 7+ parameters. Use structs or builder patterns to reduce parameter counts.
- **`large_enum_variant`**: Warns on enums where one variant is significantly larger than others. Box the large variant payload.

Note: Run `cargo clippy --fix --workspace` routinely.

## 3. Terminal User Interface (TUI) Styling
Our TUI is built on `ratatui` and relies on semantic, extension-driven styling to avoid boilerplate.

- **Do Not Use Builders**: Avoid instantiating styles explicitly via `Style::default().fg(Color::Cyan)`.
- **Use `Style::new()` with Stylize Chains**: Import `ratatui::style::Stylize`. Use chain methods:
  - `Style::new().fg(color).bold()` for computed styles
  - `"text".cyan().bold()` for inline span styling
  - `vec![...].into()` instead of manual `Span` arrays.
- **No Hardcoded Colors**: Never use `Color::Indexed(N)` or raw RGB values in rendering code. Use the `colors::` module from `theme.rs` (e.g., `colors::divider()`, `colors::border()`).
- **Dynamic Theme System**: The TUI runs an adaptive semantic palette via `ThemeConfig` (defined in `crates/crow-cli/src/tui/theme.rs`). It employs ITU-R BT.601 luminance detection via the `COLORFGBG` heuristic to automatically render light or dark modes.
  - Rely on theme constants like `theme.accent_user`, `theme.accent_system`, `theme.surface` rather than passing raw RGB colors. 
  - Ensure background blending uses `blend()` helpers instead of crude overrides.

## 4. Agent Architecture Patterns

### StreamController / CommitTick
The TUI uses a `StreamController` (`stream_controller.rs`) to buffer LLM streaming output and drain it at a controlled rate (1 line per 120ms tick). This prevents the UI from stuttering during intense token generation. The adaptive policy switches to batch draining (5 lines/tick) when the backlog exceeds 20 lines.

### Structured TurnEvent Protocol
All agent turn lifecycle transitions emit structured `TurnEvent` variants (`event.rs`):
- `TurnEvent::Started` — emitted by `ThreadManager` when a turn is spawned
- `TurnEvent::PhaseChanged` — emitted during phase transitions (Materializing → EpistemicLoop → Crucible)
- `TurnEvent::Completed` — emitted with success/failure and optional token usage
- `TurnEvent::Aborted` — emitted on user cancellation with reason

### Subagent Delegation
Subagent workers (`subagent.rs`) are bounded by:
- A 120-second hard timeout
- A delegation depth counter in the epistemic loop (prevents infinite delegation chains)
- Structured error propagation via the `SubagentEventHandler`

### Error Categorization
`BrainError` (`client.rs`) categorizes errors as transient or permanent:
- **Transient** (retryable): HTTP 429, 500, 502, 503, 529, transport errors
- **Permanent** (fatal): Auth errors (401/403), parse errors, config errors

The epistemic loop retries transient errors up to 3 times with exponential backoff (2s, 4s, 8s).

### Role Alternation
The `ConversationManager` enforces strict User→Assistant→User role alternation required by providers like Anthropic. After compaction, `fix_role_alternation()` inserts minimal placeholder messages to repair any violations.

### Recon Output Capping
Recon tool output is capped at 100KB before entering the conversation context (`MAX_RECON_CONTEXT_BYTES`), separate from the 512KB execution-level cap in the verifier. This prevents a single oversized tool result from consuming the entire context budget.

## 5. MCP Integration
MCP servers are configured via `CrowConfig.mcp_servers` and managed by `McpManager` (`mcp.rs`). MCP tool calls are intercepted in the epistemic loop and routed through the manager. Results are subject to the same 100KB context cap as other recon tools.

## 6. Workflows & Committing
- When introducing cross-workspace UI changes, update `walkthrough.md` or similar documentation inside the artifact tracking directories.
- Keep commits isolated to thematic tasks.
- If dependencies change, ensure `cargo check --workspace` and `cargo test --workspace` pass synchronously.
