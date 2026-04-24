use crate::config::CrowConfig;
use crate::event::{AgentEvent, ViewMode};
use crate::tui::state::{AppState, Cell, CellKind, TuiMessage};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

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

    let trimmed = prompt.trim();

    // Save to input history (skip slash commands)
    if !trimmed.starts_with('/') {
        state.input_history.push(prompt.clone());
    }
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
                    state.history.push(Cell {
                        kind: CellKind::Error,
                        payload: "Usage: /swarm <task description>".into(),
                    });
                } else {
                    let tm = thread_manager.clone();
                    tokio::spawn(async move {
                        tm.submit(crate::thread_manager::Op::SwarmRun(payload))
                            .await;
                    });
                    state.history.push(Cell {
                        kind: CellKind::Log,
                        payload: "Launched asynchronous Sub-Agent Swarm Worker.".into(),
                    });
                }
            }
            "help" | "?" => {
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: "/help".into(),
                });
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: [
                        "Commands:",
                        "  /help          Show this message",
                        "  /status        Workspace health",
                        "  /clear         Clear conversation and start fresh session",
                        "  /view <mode>   Set view (focus|evidence|audit)",
                        "  /model         Show current model",
                        "  /swarm <task>  Launch background sub-agent",
                        "  /compact       Force context compaction",
                        "  /diff          Show git diff (including untracked)",
                        "  /memory        Manage persistent workspace memory",
                        "  /exit          Exit Crow",
                        "",
                        "Shortcuts:",
                        "  Ctrl+C         Interrupt / quit (press twice)",
                        "  Ctrl+D         Quit immediately",
                        "  Ctrl+J         Insert newline",
                        "  Ctrl+L         Clear screen",
                        "  Ctrl+U         Clear input",
                        "  Esc            Interrupt running task",
                        "  ?              Toggle shortcut overlay",
                        "  !<cmd>         Execute shell command",
                    ]
                    .join("\n"),
                });
            }
            "status" => {
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: "/status".into(),
                });
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: format!(
                        "Model: {}\nWorkspace: {}\nWrite Mode: {}\nView: {:?}",
                        state.model_info, state.workspace_name, state.write_mode, state.view_mode,
                    ),
                });
            }
            "model" => {
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: "/model".into(),
                });
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: format!("Current model: {}", state.model_info),
                });
            }
            "view" => {
                let mode = parts.next().unwrap_or("evidence");
                state.view_mode = match mode {
                    "focus" => ViewMode::Focus,
                    "audit" => ViewMode::Audit,
                    _ => ViewMode::Evidence,
                };
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: format!("View mode: {:?}", state.view_mode),
                });
            }
            "compact" => {
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: "/compact".into(),
                });
                // Actually trigger compaction through the thread manager
                let tm = thread_manager.clone();
                let tx_c = tx.clone();
                tokio::spawn(async move {
                    // Send a special compaction prompt that the agent loop will interpret
                    tm.submit(crate::thread_manager::Op::Input(
                        "[SYSTEM: Force context compaction now. Summarize the conversation so far concisely.]".to_string()
                    )).await;
                    let _ = tx_c.send(TuiMessage::AgentEvent(
                        crate::event::AgentEvent::Log("Context compaction initiated.".into())
                    ));
                });
                state.history.push(Cell {
                    kind: CellKind::Log,
                    payload: "Compacting context window...".into(),
                });
            }
            "diff" => {
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: "/diff".into(),
                });
                // Show git diff including untracked files (Codex pattern)
                let workspace = cfg.workspace.clone();
                let tx_diff = tx.clone();
                tokio::spawn(async move {
                    let mut diff_output = String::new();

                    // Tracked changes
                    if let Ok(output) = tokio::process::Command::new("git")
                        .args(["diff", "--stat", "HEAD"])
                        .current_dir(&workspace)
                        .output()
                        .await
                    {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if !stdout.trim().is_empty() {
                            diff_output.push_str("Changes (tracked):\n");
                            diff_output.push_str(&stdout);
                        }
                    }

                    // Staged changes
                    if let Ok(output) = tokio::process::Command::new("git")
                        .args(["diff", "--stat", "--cached"])
                        .current_dir(&workspace)
                        .output()
                        .await
                    {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if !stdout.trim().is_empty() {
                            if !diff_output.is_empty() { diff_output.push('\n'); }
                            diff_output.push_str("Changes (staged):\n");
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
                            if !diff_output.is_empty() { diff_output.push('\n'); }
                            diff_output.push_str("Untracked files:\n");
                            for file in stdout.lines() {
                                diff_output.push_str(&format!("  + {file}\n"));
                            }
                        }
                    }

                    if diff_output.trim().is_empty() {
                        diff_output = "Working tree is clean.".into();
                    }

                    let _ = tx_diff.send(TuiMessage::AgentEvent(
                        crate::event::AgentEvent::Log(diff_output),
                    ));
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
                
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: display_payload,
                });

                let action = rest_args.first().copied().unwrap_or("show");

                match action {
                    "add" => {
                        let text = rest_args[1..].join(" ");
                        if text.is_empty() {
                            state.history.push(Cell {
                                kind: CellKind::Error,
                                payload: "Usage: /memory add <text>".into(),
                            });
                        } else if let Err(e) = std::fs::create_dir_all(".crow") {
                            state.history.push(Cell { kind: CellKind::Error, payload: format!("Failed to create .crow directory: {e}") });
                        } else {
                            use std::io::Write;
                            match std::fs::OpenOptions::new().create(true).append(true).open(&memory_file) {
                                Ok(mut f) => {
                                    if let Err(e) = writeln!(f, "- {text}") {
                                        state.history.push(Cell { kind: CellKind::Error, payload: format!("Failed to write to memory: {e}") });
                                    } else {
                                        state.history.push(Cell { kind: CellKind::Log, payload: "Memory added successfully.".into() });
                                    }
                                }
                                Err(e) => {
                                    state.history.push(Cell { kind: CellKind::Error, payload: format!("Failed to open memory file: {e}") });
                                }
                            }
                        }
                    }
                    "clear" => {
                        let _ = std::fs::remove_file(&memory_file);
                        state.history.push(Cell { kind: CellKind::Log, payload: "Persistent memory cleared.".into() });
                    }
                    _ => {
                        match std::fs::read_to_string(&memory_file) {
                            Ok(content) if !content.trim().is_empty() => {
                                state.history.push(Cell {
                                    kind: CellKind::Log,
                                    payload: format!("Persistent Memory:\n{content}"),
                                });
                            }
                            _ => {
                                state.history.push(Cell {
                                    kind: CellKind::Log,
                                    payload: "Memory is empty. Use '/memory add <text>' to store persistent context.".into(),
                                });
                            }
                        }
                    }
                }
            }
            "session" => {
                let action = parts.next().unwrap_or("list");
                state.history.push(Cell {
                    kind: CellKind::User,
                    payload: format!("/session {action}"),
                });

                if action == "list" {
                    match crow_runtime::session::SessionStore::open() {
                        Ok(store) => match store.list() {
                            Ok(summaries) => {
                                let mut out = String::from("Saved sessions:\n");
                                for summary in summaries.into_iter().take(10) {
                                    out.push_str(&format!("{summary}\n"));
                                }
                                state.history.push(Cell {
                                    kind: CellKind::Log,
                                    payload: out,
                                });
                            }
                            Err(e) => {
                                state.history.push(Cell {
                                    kind: CellKind::Error,
                                    payload: format!("Failed to list sessions: {e}"),
                                });
                            }
                        },
                        Err(e) => {
                            state.history.push(Cell {
                                kind: CellKind::Error,
                                payload: format!("Failed to open session store: {e}"),
                            });
                        }
                    }
                } else if action == "resume" {
                    let maybe_id = parts.next();
                    if let Some(_id) = maybe_id {
                        state.history.push(Cell {
                            kind: CellKind::Log,
                            payload: "To resume a session, restart crow using: crow -r <id>".into(),
                        });
                    } else {
                        state.history.push(Cell {
                            kind: CellKind::Error,
                            payload: "Usage: /session resume <id>".into(),
                        });
                    }
                }
            }
            other => {
                state.history.push(Cell {
                    kind: CellKind::Error,
                    payload: format!(
                        "Unknown command: /{other}. Type /help for available commands."
                    ),
                });
            }
        }
        state.composer.clear();
        state.composer_cursor = 0;
        return;
    }

    // ── Pre-execution Queue Check ────────────────────────────────────
    if state.is_task_running() {
        state.task_queue.push_back(prompt.clone());
        state.history.push(Cell {
            kind: CellKind::User,
            payload: prompt.clone(),
        });
        state.history.push(Cell {
            kind: CellKind::Log,
            payload: "Queued for execution...".into(),
        });
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
            "&&", "||", ";", "|", "$(", "${", "$", "`", ">", "<", "(", ")", "{", "}", "\n", "\\", "#",
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
            state.history.push(Cell {
                kind: CellKind::User,
                payload: format!("!{bash_cmd}"),
            });
            execute_shell_command(bash_cmd, tx.clone());
        } else {
            state.approval_state = crate::tui::state::ApprovalState::PendingCommand(bash_cmd, 0);
        }

        state.composer.clear();
        state.composer_cursor = 0;
        return;
    }

    // ── Normal prompt: send to agent ─────────────────────────────────
    state.history.push(Cell {
        kind: CellKind::User,
        payload: prompt.clone(),
    });

    state.active_action = Some("Thinking...".into());
    state.task_start_time = Some(Instant::now());

    let tm = thread_manager.clone();
    tokio::spawn(async move {
        tm.submit(crate::thread_manager::Op::Input(prompt)).await;
    });

    state.composer.clear();
    state.composer_cursor = 0;
}
