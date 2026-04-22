use crossterm::style::{Color, Stylize};
// Intentionally left blank or removed unused imports

// ── Structured Protocol Layer (SQ/EQ Pattern) ──────────────────────

pub use crow_runtime::event::{
    AgentEvent, EventHandler, TokenUsageSummary, TurnEvent, TurnPhase,
};

/// The level of detail provided to the user during the autonomous loop.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Minimal output: Goal, Action, Result.
    Focus,
    /// Detailed trace: Goal, Evidence (file reads, recon), Action, Result.
    #[default]
    Evidence,
    /// Full audit stream: Includes inner monologue / raw reasoning streams.
    Audit,
}

pub struct TuiEventHandler {
    tx: tokio::sync::mpsc::UnboundedSender<crate::tui::state::TuiMessage>,
    cancellation: Option<crate::tui::state::CancellationToken>,
}

impl TuiEventHandler {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<crate::tui::state::TuiMessage>) -> Self {
        Self {
            tx,
            cancellation: None,
        }
    }

    pub fn with_cancellation(
        tx: tokio::sync::mpsc::UnboundedSender<crate::tui::state::TuiMessage>,
        token: crate::tui::state::CancellationToken,
    ) -> Self {
        Self {
            tx,
            cancellation: Some(token),
        }
    }
}

impl EventHandler for TuiEventHandler {
    fn handle_event(&mut self, event: AgentEvent) {
        let _ = self
            .tx
            .send(crate::tui::state::TuiMessage::AgentEvent(event));
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(super::tui::state::CancellationToken::is_cancelled)
    }
}

/// CLI event handler with spinner-based progress feedback.
///
/// Refactored for 'Crow Console 2.0' (Evidence-Native).
pub struct CliEventHandler {
    spinner: Option<crate::epistemic_ui::SpinnerObserver>,
    stream_char_count: usize,
    view_mode: ViewMode,
}

impl Default for CliEventHandler {
    fn default() -> Self {
        Self::new(ViewMode::default())
    }
}

impl CliEventHandler {
    pub fn new(view_mode: ViewMode) -> Self {
        Self {
            spinner: None,
            stream_char_count: 0,
            view_mode,
        }
    }

    fn stop_spinner(&mut self) {
        if let Some(sp) = self.spinner.take() {
            sp.finish();
        }
    }

    fn sync_print<F: FnOnce()>(&self, f: F) {
        if let Some(sp) = &self.spinner {
            sp.suspend(f);
        } else {
            f();
        }
    }

    fn print_trace(&self, label: &str, body: &str, accent: Color) {
        self.sync_print(|| {
            let icon = match label {
                "Evidence" => "◎",
                "Action" => "▰",
                "Result" => "✓",
                _ => "•",
            };

            println!(
                "  {} {} {:8} {}",
                "│ ".with(Color::DarkGrey),
                icon.with(accent),
                label.with(accent).bold(),
                body.with(Color::AnsiValue(245))
            );
        });
    }

    fn update_spinner(&mut self, text: String) {
        if let Some(sp) = &mut self.spinner {
            sp.set_pattern(text);
        } else {
            self.spinner = Some(crate::epistemic_ui::SpinnerObserver::new(text));
        }
    }
}

impl Drop for CliEventHandler {
    fn drop(&mut self) {
        self.stop_spinner();
    }
}

impl EventHandler for CliEventHandler {
    fn handle_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Turn(turn_ev) => match turn_ev {
                TurnEvent::Started { turn_id } => {
                    if self.view_mode == ViewMode::Audit {
                        self.print_trace(
                            "Turn",
                            &format!("Started [{turn_id}]"),
                            Color::AnsiValue(117),
                        );
                    }
                }
                TurnEvent::Completed {
                    turn_id, success, ..
                } => {
                    if self.view_mode == ViewMode::Audit {
                        let status = if success { "✓" } else { "✘" };
                        self.print_trace(
                            "Turn",
                            &format!("{status} Completed [{turn_id}]"),
                            Color::AnsiValue(117),
                        );
                    }
                }
                TurnEvent::Aborted { turn_id, reason } => {
                    self.print_trace(
                        "Turn",
                        &format!("Aborted [{turn_id}]: {reason}"),
                        Color::AnsiValue(203),
                    );
                }
                TurnEvent::PhaseChanged { phase, .. } => {
                    if self.view_mode == ViewMode::Audit {
                        self.print_trace("Phase", &format!("{phase}"), Color::AnsiValue(245));
                    }
                }
            },
            AgentEvent::Thinking(_step, _max) => {
                self.stop_spinner();
                self.stream_char_count = 0;

                self.spinner = Some(crate::epistemic_ui::SpinnerObserver::new(
                    "Thinking...".to_string(),
                ));
            }
            AgentEvent::StreamChunk(chunk) => {
                self.stream_char_count += chunk.len();

                if self.view_mode == ViewMode::Audit {
                    self.sync_print(|| {
                        use std::io::Write;
                        print!("{}", chunk.with(Color::AnsiValue(242)));
                        let _ = std::io::stdout().flush();
                    });
                }

                // We completely swallow rationale rendering to hide internal monologue.
                // We just keep the spinner updating to show flow health.
                if let Some(ref mut sp) = self.spinner {
                    let kb = self.stream_char_count as f64 / 1024.0;
                    sp.set_status(format!("{kb:.1} KB transferred"));
                }
            }
            AgentEvent::ActionStart(desc) => {
                self.update_spinner(format!("Running action: {desc}"));
            }
            AgentEvent::ActionComplete(desc) => {
                self.print_trace("Action", &desc, Color::AnsiValue(114));
            }
            AgentEvent::ReadFiles(paths) => {
                if self.view_mode != ViewMode::Focus {
                    let display = if paths.len() <= 3 {
                        paths.join(", ")
                    } else {
                        format!("{}, ... ({} files)", paths[..2].join(", "), paths.len())
                    };
                    self.print_trace(
                        "Evidence",
                        &format!("Read {display}"),
                        Color::AnsiValue(245),
                    );
                }
            }
            AgentEvent::ReconStart(desc) => {
                if self.view_mode != ViewMode::Focus {
                    self.update_spinner(format!("Recon: {desc}"));
                }
            }
            AgentEvent::DelegateStart(_, task) => {
                if self.view_mode != ViewMode::Focus {
                    self.update_spinner(format!("Delegating: {task}"));
                }
            }
            AgentEvent::DelegateComplete(_, _) => {}
            AgentEvent::PlanSubmitted(plan) => {
                if !plan.operations.is_empty() {
                    let summary = format!("{} operations generated", plan.operations.len());
                    self.print_trace("Action", &summary, Color::AnsiValue(81));
                }
            }
            AgentEvent::CruciblePreflight(msg) => {
                self.update_spinner(format!("Verifying: {msg}"));
            }
            AgentEvent::Log(msg) => {
                if msg.contains("⚠") {
                    self.sync_print(|| {
                        println!(
                            "  {} {}",
                            "│ ".with(Color::DarkGrey),
                            msg.with(Color::Yellow)
                        )
                    });
                } else if !msg.starts_with("       Rationale:") || self.view_mode == ViewMode::Audit
                {
                    if msg.starts_with("✓ ") || msg.starts_with("↳") {
                        self.print_trace(
                            "Result",
                            msg.trim_start_matches("✓ ").trim_start_matches("↳").trim(),
                            Color::AnsiValue(245),
                        );
                    } else if self.view_mode != ViewMode::Focus {
                        self.print_trace("Log", &msg, Color::AnsiValue(245));
                    }
                }
            }
            AgentEvent::Error(err) => {
                self.stop_spinner();
                self.sync_print(|| {
                    eprintln!(
                        "  {} {} {}",
                        "│ ".with(Color::DarkGrey),
                        "✘".bold().with(Color::AnsiValue(203)),
                        err.with(Color::AnsiValue(203))
                    );
                });
            }
            AgentEvent::Markdown(md) => {
                let renderer = crate::render::TerminalRenderer::new();
                println!();
                renderer.print_markdown(&md);
                println!();
            }
            AgentEvent::TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                context_window,
            } => {
                if self.view_mode == ViewMode::Audit {
                    let pct = (total_tokens as u64 * 100)
                        .checked_div(context_window as u64)
                        .unwrap_or(0);
                    self.print_trace(
                        "Tokens",
                        &format!("{prompt_tokens}+{completion_tokens}={total_tokens} ({pct}% of {context_window})"),
                        Color::AnsiValue(117),
                    );
                }
            }
            AgentEvent::StateChanged { from, to } => {
                if self.view_mode == ViewMode::Audit {
                    self.print_trace("State", &format!("{from} → {to}"), Color::AnsiValue(245));
                }
            }
            AgentEvent::Retrying {
                attempt,
                max_attempts,
                reason,
            } => {
                self.update_spinner(format!("Retrying ({attempt}/{max_attempts})… {reason}"));
            }
            AgentEvent::Compacting { active } => {
                if active {
                    self.update_spinner("Compacting context history…".to_string());
                } else {
                    self.print_trace(
                        "Action",
                        "Context compaction complete",
                        Color::AnsiValue(114),
                    );
                }
            }
            AgentEvent::ToolProgress {
                tool_id: _,
                message,
            } => {
                if self.view_mode != ViewMode::Focus {
                    self.update_spinner(message);
                }
            }
        }
    }
}
