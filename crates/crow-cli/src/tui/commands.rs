use crate::config::CrowConfig;
use crate::event::{AgentEvent, ViewMode};
use crate::tui::state::{AppState, TuiMessage};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

// ── Slash Command Registry ──────────────────────────────────────────────────

/// Slash command definition for autocomplete and help rendering.
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub usage: &'static str,
}

/// All available slash commands.
pub fn all_commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand {
            name: "help",
            description: "Show available commands",
            usage: "/help",
        },
        SlashCommand {
            name: "status",
            description: "Show session status (model, workspace, view mode)",
            usage: "/status",
        },
        SlashCommand {
            name: "model",
            description: "Switch model/provider",
            usage: "/model <provider>",
        },
        SlashCommand {
            name: "clear",
            description: "Clear conversation and start fresh session",
            usage: "/clear",
        },
        SlashCommand {
            name: "compact",
            description: "Force context compaction",
            usage: "/compact",
        },
        SlashCommand {
            name: "auto",
            description: "Run a task with auto-mode orchestration",
            usage: "/auto <task>",
        },
        SlashCommand {
            name: "agent",
            description: "Show auto-mode agent status",
            usage: "/agent",
        },
        SlashCommand {
            name: "agents",
            description: "Show auto-mode agent status",
            usage: "/agents",
        },
        SlashCommand {
            name: "diff",
            description: "Show git diff (including untracked)",
            usage: "/diff",
        },
        SlashCommand {
            name: "undo",
            description: "Revert workspace to clean state",
            usage: "/undo",
        },
        SlashCommand {
            name: "tokens",
            description: "Show context window usage",
            usage: "/tokens",
        },
        SlashCommand {
            name: "cost",
            description: "Show token usage and estimated cost",
            usage: "/cost",
        },
        SlashCommand {
            name: "copy",
            description: "Copy last agent message to clipboard",
            usage: "/copy",
        },
        SlashCommand {
            name: "view",
            description: "Set view mode (focus|evidence|audit)",
            usage: "/view <mode>",
        },
        SlashCommand {
            name: "memory",
            description: "Manage persistent workspace memory",
            usage: "/memory [add|clear|show]",
        },
        SlashCommand {
            name: "session",
            description: "List or resume saved sessions",
            usage: "/session [list|resume <id>]",
        },
        SlashCommand {
            name: "swarm",
            description: "Launch background sub-agent",
            usage: "/swarm <task>",
        },
        SlashCommand {
            name: "exit",
            description: "Exit the application",
            usage: "/exit",
        },
        SlashCommand {
            name: "quit",
            description: "Exit the application",
            usage: "/quit",
        },
    ]
}

/// Get autocomplete suggestions for a partial command input.
///
/// The input may or may not start with `/`; the leading slash is stripped
/// before matching. Returns all commands whose name starts with the
/// remaining prefix.
pub fn autocomplete_commands(partial: &str) -> Vec<SlashCommand> {
    let needle = partial.trim_start_matches('/');
    all_commands()
        .into_iter()
        .filter(|c| c.name.starts_with(needle))
        .collect()
}

pub fn execute_shell_command(bash_cmd: String, tx: mpsc::UnboundedSender<TuiMessage>) {
    tokio::spawn(async move {
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&bash_cmd)
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout_stripped = strip_ansi_escapes::strip(&out.stdout);
                let stderr_stripped = strip_ansi_escapes::strip(&out.stderr);
                let stdout = String::from_utf8_lossy(&stdout_stripped).into_owned();
                let stderr = String::from_utf8_lossy(&stderr_stripped).into_owned();
                let mut report = stdout;
                if !stderr.is_empty() {
                    if !report.is_empty() {
                        report.push('\n');
                    }
                    report.push_str(&stderr);
                }
                if report.trim().is_empty() {
                    report = "(no output)".into();
                }
                let _ = tx.send(TuiMessage::AgentEvent(AgentEvent::Log(report)));
            }
            Err(e) => {
                let _ = tx.send(TuiMessage::AgentEvent(AgentEvent::Error(format!(
                    "Failed: {e}"
                ))));
            }
        }
        let _ = tx.send(TuiMessage::SessionComplete);
    });
}

pub fn handle_enter(
    state: &mut AppState,
    tx: &mpsc::UnboundedSender<TuiMessage>,
    cfg: &CrowConfig,
    thread_manager: &Arc<crate::thread_manager::ThreadManager>,
) {
    let prompt = state.composer.clone();
    if prompt.trim().is_empty() {
        return;
    }

    // Save to input history (all commands, including slash commands)
    state.input_history.push(prompt.clone());
    state.input_history_idx = None;
    state.scroll_offset = 0;

    execute_command_string(state, prompt, tx, cfg, thread_manager);
}

pub fn execute_command_string(
    state: &mut AppState,
    prompt: String,
    tx: &mpsc::UnboundedSender<TuiMessage>,
    cfg: &CrowConfig,
    thread_manager: &Arc<crate::thread_manager::ThreadManager>,
) {
    let trimmed = prompt.trim();

    // ── Slash commands ───────────────────────────────────────────────
    if trimmed.starts_with('/') {
        let mut parts = trimmed.trim_start_matches('/').split_whitespace();
        let cmd = parts.next().unwrap_or_default();
        match cmd {
            "exit" | "quit" | "q" => {
                state.composer.clear();
                state.composer_cursor = 0;
                let _ = tx.send(TuiMessage::Quit);
            }
            "clear" | "c" => {
                state.history.clear();
                let tm = thread_manager.clone();
                tokio::spawn(async move {
                    tm.submit(crate::thread_manager::Op::Clear).await;
                });
            }
            "swarm" => {
                let payload = parts.collect::<Vec<_>>().join(" ");
                if payload.is_empty() {
                    state.push_error("Usage: /swarm <task description>");
                } else {
                    let tm = thread_manager.clone();
                    tokio::spawn(async move {
                        tm.submit(crate::thread_manager::Op::SwarmRun(payload))
                            .await;
                    });
                    state.push_log("Launched asynchronous Sub-Agent Swarm Worker.");
                }
            }
            "auto" => {
                let payload = parts.collect::<Vec<_>>().join(" ");
                if payload.is_empty() {
                    state.push_error("Usage: /auto <task description>");
                } else {
                    state.push_user(format!("/auto {payload}"));
                    state.active_action = Some("Auto mode starting…".into());
                    state.task_start_time = Some(Instant::now());
                    let tm = thread_manager.clone();
                    tokio::spawn(async move {
                        tm.submit(crate::thread_manager::Op::Auto(payload)).await;
                    });
                }
            }
            "agents" | "agent" => {
                state.push_user(format!("/{cmd}"));
                state.push_result(state.format_agent_hud_summary());
            }
            "help" | "?" => {
                state.push_user("/help");
                let mut help_text = String::from("Commands:\n");
                for cmd in all_commands() {
                    help_text.push_str(&format!("  {:<14}{}", cmd.usage, cmd.description));
                    help_text.push('\n');
                }
                help_text.push_str("\nShortcuts:\n");
                help_text.push_str("  Ctrl+C         Interrupt / quit (press twice)\n");
                help_text.push_str("  Ctrl+D         Quit immediately\n");
                help_text.push_str("  Ctrl+J         Insert newline\n");
                help_text.push_str("  Ctrl+L         Clear screen\n");
                help_text.push_str("  Ctrl+U         Clear input\n");
                help_text.push_str("  Esc            Interrupt running task\n");
                help_text.push_str("  ?              Toggle shortcut overlay\n");
                help_text.push_str("  !<cmd>         Execute shell command");
                state.push_log(help_text);
            }
            "status" => {
                state.push_user("/status");
                let session_duration = state.session_start.elapsed();
                let mins = session_duration.as_secs() / 60;
                let secs = session_duration.as_secs() % 60;
                let total_tokens =
                    state.cumulative_prompt_tokens + state.cumulative_completion_tokens;
                let turns = state
                    .history
                    .iter()
                    .filter(|c| c.kind_label() == "User")
                    .count();
                state.push_log(format!(
                    "Session Status:\n  Model:      {}\n  Workspace:  {}\n  Write Mode: {}\n  View:       {:?}\n  Git Branch: {}\n  Duration:   {mins}m {secs}s\n  Turns:      {turns}\n  Tokens:     {total_tokens}",
                    state.model_info, state.workspace_name, state.write_mode,
                    state.view_mode, state.git_branch,
                ));
            }
            "model" => {
                if let Some(provider) = parts.next() {
                    let provider = provider.trim();
                    match crate::config::CrowConfig::set_llm_provider(&cfg.workspace, provider) {
                        Ok(_) => {
                            state.push_user(format!("/model {provider}"));
                            state.push_log(format!("✅ Model provider updated to '{provider}'. Restart crow or create a new turn to take effect."));
                        }
                        Err(e) => {
                            state.push_user(format!("/model {provider}"));
                            state.push_error(format!("Failed to set model: {e}"));
                        }
                    }
                } else {
                    state.push_user("/model");
                    state.push_log(format!("Current model: {}\nUsage: /model <provider> (e.g. /model kimi, /model claude, /model qwen)", state.model_info));
                }
            }
            "view" => {
                let mode = parts.next().unwrap_or("evidence");
                state.view_mode = match mode {
                    "focus" => ViewMode::Focus,
                    "audit" => ViewMode::Audit,
                    _ => ViewMode::Evidence,
                };
                state.push_log(format!("View mode: {:?}", state.view_mode));
            }
            "compact" => {
                state.push_user("/compact");
                // Actually trigger compaction through the thread manager
                let tm = thread_manager.clone();
                let token = crate::tui::state::CancellationToken::new();
                state.cancellation = Some(token.clone());
                tokio::spawn(async move {
                    tm.submit(crate::thread_manager::Op::Compact(token)).await;
                });
            }
            "diff" => {
                state.push_user("/diff");
                // Show actual git diff content with syntax-highlighted rendering
                let workspace = cfg.workspace.clone();
                let tx_diff = tx.clone();
                tokio::spawn(async move {
                    let mut diff_output = String::new();

                    // Unstaged changes (full unified diff)
                    if let Ok(output) = tokio::process::Command::new("git")
                        .args(["diff", "HEAD"])
                        .current_dir(&workspace)
                        .output()
                        .await
                    {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if !stdout.trim().is_empty() {
                            diff_output.push_str(&stdout);
                        }
                    }

                    // Staged changes (full unified diff)
                    if let Ok(output) = tokio::process::Command::new("git")
                        .args(["diff", "--cached"])
                        .current_dir(&workspace)
                        .output()
                        .await
                    {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if !stdout.trim().is_empty() {
                            if !diff_output.is_empty() {
                                diff_output.push('\n');
                            }
                            diff_output.push_str("── Staged ──\n");
                            diff_output.push_str(&stdout);
                        }
                    }

                    // Untracked files
                    if let Ok(output) = tokio::process::Command::new("git")
                        .args(["ls-files", "--others", "--exclude-standard"])
                        .current_dir(&workspace)
                        .output()
                        .await
                    {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if !stdout.trim().is_empty() {
                            if !diff_output.is_empty() {
                                diff_output.push('\n');
                            }
                            diff_output.push_str("── Untracked ──\n");
                            for file in stdout.lines() {
                                diff_output.push_str(&format!("  + {file}\n"));
                            }
                        }
                    }

                    if diff_output.trim().is_empty() {
                        diff_output = "Working tree is clean.".into();
                    }

                    // Send as a special diff event so we can render it with DiffCell
                    let _ = tx_diff.send(TuiMessage::AgentEvent(crate::event::AgentEvent::Log(
                        format!("__DIFF__{diff_output}"),
                    )));
                });
            }
            "memory" => {
                let memory_file = std::path::Path::new(".crow").join("memory.md");
                let rest_args: Vec<_> = parts.collect();
                let display_payload = if rest_args.is_empty() {
                    "/memory".to_string()
                } else {
                    format!("/memory {}", rest_args.join(" "))
                };

                state.push_user(display_payload);

                let action = rest_args.first().copied().unwrap_or("show");

                match action {
                    "add" => {
                        let text = rest_args[1..].join(" ");
                        if text.is_empty() {
                            state.push_error("Usage: /memory add <text>");
                        } else if let Err(e) = std::fs::create_dir_all(".crow") {
                            state.push_error(format!("Failed to create .crow directory: {e}"));
                        } else {
                            use std::io::Write;
                            match std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&memory_file)
                            {
                                Ok(mut f) => {
                                    if let Err(e) = writeln!(f, "- {text}") {
                                        state.push_error(format!("Failed to write to memory: {e}"));
                                    } else {
                                        state.push_log("Memory added successfully.");
                                    }
                                }
                                Err(e) => {
                                    state.push_error(format!("Failed to open memory file: {e}"));
                                }
                            }
                        }
                    }
                    "clear" => {
                        let _ = std::fs::remove_file(&memory_file);
                        state.push_log("Persistent memory cleared.");
                    }
                    _ => match std::fs::read_to_string(&memory_file) {
                        Ok(content) if !content.trim().is_empty() => {
                            state.push_log(format!("Persistent Memory:\n{content}"));
                        }
                        _ => {
                            state.push_log("Memory is empty. Use '/memory add <text>' to store persistent context.");
                        }
                    },
                }
            }
            "session" => {
                let action = parts.next().unwrap_or("list");
                state.push_user(format!("/session {action}"));

                if action == "list" {
                    match crow_runtime::session::SessionStore::open() {
                        Ok(store) => match store.list() {
                            Ok(summaries) => {
                                let mut out = String::from("Saved sessions:\n");
                                for summary in summaries.into_iter().take(10) {
                                    out.push_str(&format!("{summary}\n"));
                                }
                                state.push_log(out);
                            }
                            Err(e) => {
                                state.push_error(format!("Failed to list sessions: {e}"));
                            }
                        },
                        Err(e) => {
                            state.push_error(format!("Failed to open session store: {e}"));
                        }
                    }
                } else if action == "resume" {
                    let maybe_id = parts.next();
                    if maybe_id.is_some() {
                        state.push_log("To resume a session, restart crow using: crow -r <id>");
                    } else {
                        state.push_error("Usage: /session resume <id>");
                    }
                }
            }
            "tokens" => {
                state.push_user("/tokens");
                if let Some((total_tokens, context_window)) = state.ctx_usage {
                    let pct = if context_window > 0 {
                        (f64::from(total_tokens) / f64::from(context_window) * 100.0) as u32
                    } else {
                        0
                    };
                    let remaining = context_window.saturating_sub(total_tokens);
                    state.push_log(format!(
                        "Context Window Usage:\n  Used:      {total_tokens} tokens\n  Remaining: {remaining} tokens\n  Window:    {context_window} tokens\n  Usage:     {pct}%"
                    ));
                } else {
                    state.push_log("Context usage not yet available — send a message first.");
                }
            }
            "cost" => {
                state.push_user("/cost");
                let prompt_tok = state.cumulative_prompt_tokens;
                let completion_tok = state.cumulative_completion_tokens;
                let total = prompt_tok + completion_tok;
                if total == 0 {
                    state.push_log(
                        "No token usage recorded yet. Usage tracking requires API responses with usage data."
                    );
                } else {
                    state.push_log(format!(
                        "Session Token Usage:\n  Prompt:     {prompt_tok} tokens\n  Completion: {completion_tok} tokens\n  Total:      {total} tokens\n  Model:      {}",
                        state.model_info
                    ));
                }
            }
            "undo" => {
                state.push_user("/undo");
                let workspace = cfg.workspace.clone();
                let tx_undo = tx.clone();
                tokio::spawn(async move {
                    // Check for uncommitted changes first
                    let status_output = tokio::process::Command::new("git")
                        .args(["status", "--porcelain"])
                        .current_dir(&workspace)
                        .output()
                        .await;

                    match status_output {
                        Ok(output) => {
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            if stdout.trim().is_empty() {
                                let _ = tx_undo.send(TuiMessage::AgentEvent(AgentEvent::Log(
                                    "No changes to undo — working tree is clean.".into(),
                                )));
                                return;
                            }

                            // Restore all tracked (modified/deleted) files
                            let checkout = tokio::process::Command::new("git")
                                .args(["checkout", "HEAD", "--", "."])
                                .current_dir(&workspace)
                                .output()
                                .await;

                            // Clean untracked files that were added
                            let clean = tokio::process::Command::new("git")
                                .args(["clean", "-fd"])
                                .current_dir(&workspace)
                                .output()
                                .await;

                            let mut report = String::from("✅ Reverted workspace changes:\n");
                            // Count what was reverted
                            for line in stdout.lines() {
                                let status = line.get(0..2).unwrap_or("  ");
                                let file = line.get(3..).unwrap_or("?");
                                let action = match status.trim() {
                                    "M" => "restored",
                                    "A" | "??" => "deleted",
                                    "D" => "restored",
                                    _ => "reverted",
                                };
                                report.push_str(&format!("  {action}: {file}\n"));
                            }

                            if checkout.is_err() || clean.is_err() {
                                report.push_str("\n⚠ Some files may not have been fully reverted.");
                            }

                            let _ = tx_undo.send(TuiMessage::AgentEvent(AgentEvent::Log(report)));
                        }
                        Err(e) => {
                            let _ = tx_undo.send(TuiMessage::AgentEvent(AgentEvent::Error(
                                format!("Failed to check git status: {e}"),
                            )));
                        }
                    }
                });
            }
            "copy" => {
                state.push_user("/copy");
                let last_agent_text = state
                    .history
                    .iter()
                    .rev()
                    .find(|c| c.kind_label() == "Agent")
                    .map(|c| c.raw_text().to_string());
                if let Some(text) = last_agent_text {
                    state.push_log(format!(
                        "Captured last agent message ({} chars). Use terminal copy to paste.",
                        text.len()
                    ));
                } else {
                    state.push_error("No agent messages to copy.");
                }
            }
            other => {
                state.push_error(format!(
                    "Unknown command: /{other}. Type /help for available commands."
                ));
            }
        }
        state.composer.clear();
        state.composer_cursor = 0;
        return;
    }

    // ── Pre-execution Queue Check ────────────────────────────────────
    if state.is_task_running() {
        state.task_queue.push_back(prompt.clone());
        state.push_user(prompt.clone());
        state.push_log("Queued for execution...");
        state.composer.clear();
        state.composer_cursor = 0;
        return;
    }

    // ── Shell commands (!cmd) ────────────────────────────────────────
    if trimmed.starts_with('!') {
        let bash_cmd = trimmed.trim_start_matches('!').trim().to_string();

        let safe_prefixes = [
            "ls",
            "pwd",
            "echo",
            "cat",
            "git status",
            "git branch",
            "git diff",
            "git log",
            "git show",
            "whoami",
            "date",
            "tree",
            "hostname",
            "cargo check",
            "cargo build",
            "cargo test",
        ];

        // SECURITY: Reject commands with shell metacharacters from the fast
        // path. Execution goes through `sh -c`, so `!cargo test && curl ...`
        // would bypass the prefix allowlist without this check.
        const SHELL_METACHARACTERS: &[&str] = &[
            "&&", "||", ";", "|", "$(", "${", "$", "`", ">", "<", "(", ")", "{", "}", "\n", "\\",
            "#",
        ];
        let has_metacharacters = SHELL_METACHARACTERS
            .iter()
            .any(|meta| bash_cmd.contains(meta));

        let prefix_matches = safe_prefixes
            .iter()
            .any(|safe| bash_cmd == *safe || bash_cmd.starts_with(&format!("{safe} ")))
            || state
                .allowed_safe_patterns
                .iter()
                .any(|safe| bash_cmd == *safe || bash_cmd.starts_with(&format!("{safe} ")));

        let is_safe = prefix_matches && !has_metacharacters;

        if is_safe {
            state.push_user(format!("!{bash_cmd}"));
            execute_shell_command(bash_cmd, tx.clone());
        } else {
            state.approval_state = crate::tui::state::ApprovalState::PendingCommand(bash_cmd, 0);
        }

        state.composer.clear();
        state.composer_cursor = 0;
        return;
    }

    // ── Normal prompt: send to agent ─────────────────────────────────
    state.push_user(prompt.clone());

    state.active_action = Some("Thinking...".into());
    state.task_start_time = Some(Instant::now());

    let tm = thread_manager.clone();
    tokio::spawn(async move {
        tm.submit(crate::thread_manager::Op::Input(prompt)).await;
    });

    state.composer.clear();
    state.composer_cursor = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_list_includes_auto_and_agent() {
        let names: std::collections::HashSet<_> =
            all_commands().into_iter().map(|c| c.name).collect();
        assert!(names.contains(&"auto"));
        assert!(names.contains(&"agent"));
        assert!(names.contains(&"agents"));
    }
}
