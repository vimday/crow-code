# Developer & Agent Guidelines

Welcome to the `crow-code` repository. This document outlines the architectural boundaries, coding styles, and workflow conventions for all developers and autonomous agents contributing to this project. 

These rules draw heavily from professional agentic environments (inspired by Codex) to keep this repository clean, predictable, and robust.

---

## 1. Safety and Execution Boundaries
- **No Direct Shell Overrides**: Never use root-level or out-of-bounds shell commands without the user's explicit consent.
- **MCTS Stability**: The `crow-brain` MCTS engine has explicit parallelism timeouts. Do not remove or bypass the `180s` global exploration limit or the `120s` branch-level `tokio::time::timeout` limits. These were added to prevent deadlocks and network-induced hanging.
- **Build Isolation**: Avoid polluting the host workspace when testing patches. If executing `diff` or `git patch` internally, ensure they strictly run against the sandbox environment first.

## 2. Rust Conventions & Lints
We use a highly aggressive `[workspace.lints.clippy]` block inherited across all crates. 
If `cargo check` fails on Lints, fix them rather than ignoring them.

- **`uninlined_format_args`**: Always inline format arguments (`format!("hello {world}")` instead of `format!("hello {}", world)`).
- **`collapsible_if`**: Collapse nested `if/else` statements.
- **`redundant_closure_for_method_calls`**: Pass methods directly where applicable.
- **`unwrap_used`**: The workspace currently warns on `.unwrap()`. In core engine modules (`crow-brain`, `crow-patch`, `crow-verifier`), handle errors natively returning `Result::Err`. Only use `unwrap()` defensively inside TUI layout operations if error boundaries are truly infallible.

Note: Run `cargo clippy --fix --workspace` routinely.

## 3. Terminal User Interface (TUI) Styling
Our TUI is built on `ratatui` and relies on semantic, extension-driven styling to avoid boilerplate.

- **Do Not Use Builders**: Avoid instantiating styles explicitly via `Style::default().fg(Color::Cyan)`.
- **Use `Stylize` Helpers**: Import `ratatui::style::Stylize`. Use terse trailing methods:
  - `"text".cyan().bold()`
  - `"text".dark_gray()`
  - `vec![...].into()` instead of manual `Span` arrays.
- **Dynamic Theme System**: The TUI runs an adaptive semantic palette via `ThemeConfig` (defined in `crates/crow-cli/src/tui/theme.rs`). It employs ITU-R BT.601 luminance detection via the `COLORFGBG` heuristic to automatically render light or dark modes.
  - Rely on theme constants like `theme.accent_user`, `theme.accent_system`, `theme.surface` rather than passing raw RGB colors. 
  - Ensure background blending uses `blend()` helpers instead of crude overrides.

## 4. Workflows & Committing
- When introducing cross-workspace UI changes, update `walkthrough.md` or similar documentation inside the artifact tracking directories.
- Keep commits isolated to thematic tasks.
- If dependencies change, ensure `cargo check --workspace` and `cargo test --workspace` pass synchronously.
