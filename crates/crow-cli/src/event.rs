use crossterm::style::{Color, Stylize};
use crow_patch::IntentPlan;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Agent is analyzing the codebase and thinking.
    Thinking(u32, u32),

    /// Agent emitted a piece of text (e.g. rationale).
    StreamChunk(String),

    /// Agent decided to start a specific action.
    ActionStart(String),

    /// Agent finished an action.
    ActionComplete(String),

    /// Agent successfully built a plan.
    PlanSubmitted(IntentPlan),

    /// The crucible sandbox has started to test the plan.
    CruciblePreflight(String),

    /// Agent is reading files from the workspace.
    ReadFiles(Vec<String>),

    /// Agent is performing reconnaissance.
    ReconStart(String),

    /// Agent delegated a task to a subagent.
    DelegateStart(String),

    /// A general informational log.
    Log(String),

    /// A fatal error occurred during the loop.
    Error(String),
}

/// A receiver trait for AgentEvents, separating the engine from TUI/CLI rendering.
pub trait EventHandler: Send {
    fn handle_event(&mut self, event: AgentEvent);
}

/// CLI event handler with spinner-based progress feedback.
///
/// During model generation (tool calls / structured JSON), shows a spinner
/// with a character counter. Discrete actions (file reads, recon, delegation)
/// produce concise one-line output. The actual rendered markdown response
/// happens after the IntentCompiler returns the parsed plan.
pub struct CliEventHandler {
    spinner: Option<crate::epistemic_ui::SpinnerObserver>,
    stream_char_count: usize,

    // Live Streaming State
    rationale_processor: RationaleStreamProcessor,
    markdown_state: crate::render::MarkdownStreamState,
    renderer: crate::render::TerminalRenderer,
    streaming_markdown_started: bool,
}

impl Default for CliEventHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl CliEventHandler {
    pub fn new() -> Self {
        Self {
            spinner: None,
            stream_char_count: 0,
            rationale_processor: RationaleStreamProcessor::default(),
            markdown_state: crate::render::MarkdownStreamState::default(),
            renderer: crate::render::TerminalRenderer::new(),
            streaming_markdown_started: false,
        }
    }

    fn stop_spinner(&mut self) {
        if let Some(sp) = self.spinner.take() {
            sp.finish();
        }

        self.finish_stream_block();
    }

    fn sync_print<F: FnOnce()>(&self, f: F) {
        if let Some(sp) = &self.spinner {
            sp.suspend(f);
        } else {
            f();
        }
    }

    fn ensure_stream_block(&mut self) {
        if self.streaming_markdown_started {
            return;
        }
        self.sync_print(|| {
            println!(); // Blank line before AI response
        });
        self.streaming_markdown_started = true;
    }

    fn finish_stream_block(&mut self) {
        if let Some(final_md) = self.markdown_state.flush(&self.renderer) {
            self.ensure_stream_block();
            self.sync_print(|| {
                print!("{final_md}");
                if !final_md.ends_with('\n') {
                    println!();
                }
            });
        }

        if self.streaming_markdown_started {
            self.streaming_markdown_started = false;
        }
    }

    fn print_trace(&self, icon: &str, body: &str, accent: Color) {
        self.sync_print(|| {
            println!(
                "  {} {}",
                icon.with(accent),
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

impl EventHandler for CliEventHandler {
    fn handle_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Thinking(_step, _max) => {
                self.stop_spinner();
                self.stream_char_count = 0;
                self.rationale_processor = RationaleStreamProcessor::default();
                self.markdown_state = crate::render::MarkdownStreamState::default();

                self.spinner = Some(crate::epistemic_ui::SpinnerObserver::new("Thinking...".to_string()));
            }
            AgentEvent::StreamChunk(chunk) => {
                self.stream_char_count += chunk.len();

                // Extract unescaped rationale text incrementally
                let extracted_text = self.rationale_processor.push(&chunk);

                if !extracted_text.is_empty() {
                    // Turn off the spinner so markdown can render cleanly
                    if let Some(sp) = self.spinner.take() {
                        sp.finish();
                    }

                    self.ensure_stream_block();
                    if let Some(rendered) =
                        self.markdown_state.push(&self.renderer, &extracted_text)
                    {
                        self.sync_print(|| {
                            use std::io::Write;
                            print!("{}", rendered);
                            let _ = std::io::stdout().flush();
                        });
                    }
                } else if let Some(ref mut sp) = self.spinner {
                    let kb = self.stream_char_count as f64 / 1024.0;
                    sp.set_status(format!("{:.1} KB streamed", kb));
                }
            }
            AgentEvent::ActionStart(desc) => {
                self.update_spinner(format!("Running action: {}", desc));
            }
            AgentEvent::ActionComplete(desc) => {
                self.print_trace("✓", &desc, Color::AnsiValue(114));
            }
            AgentEvent::ReadFiles(paths) => {
                let display = if paths.len() <= 3 {
                    paths.join(", ")
                } else {
                    format!("{}, ... ({} files)", paths[..2].join(", "), paths.len())
                };
                self.print_trace("✓", &format!("Read {}", display), Color::AnsiValue(245));
            }
            AgentEvent::ReconStart(desc) => {
                self.update_spinner(format!("Recon: {}", desc));
            }
            AgentEvent::DelegateStart(task) => {
                self.update_spinner(format!("Delegating: {}", task));
            }
            AgentEvent::PlanSubmitted(plan) => {
                if !plan.operations.is_empty() {
                    let summary = format!(
                        "Plan ready · {} operations",
                        plan.operations.len()
                    );
                    self.print_trace("✓", &summary, Color::AnsiValue(114));
                }
            }
            AgentEvent::CruciblePreflight(msg) => {
                self.update_spinner(format!("Verifying: {}", msg));
            }
            AgentEvent::Log(msg) => {
                if let Some(rationale) = msg.strip_prefix("       Rationale: ") {
                    self.print_trace("↳", rationale, Color::AnsiValue(242));
                } else if msg.contains("⚠") {
                    self.sync_print(|| println!("  {}", msg.with(Color::Yellow)));
                } else {
                    self.sync_print(|| println!("  {}", msg.with(Color::AnsiValue(245))));
                }
            }
            AgentEvent::Error(err) => {
                self.stop_spinner();
                self.sync_print(|| {
                    eprintln!(
                        "  {} {}",
                        "✘".bold().with(Color::AnsiValue(203)),
                        err.with(Color::AnsiValue(203))
                    );
                });
            }
        }
    }
}


// ─── JSON Rationale Streaming ────────────────────────────────────────

/// Incrementally extracts the "rationale" string from a streamed JSON response.
#[derive(Default)]
struct RationaleStreamProcessor {
    buffer: String,
    yielded_bytes: usize,
    found_start: Option<usize>,
    escaping: bool,
    finished: bool,
}

impl RationaleStreamProcessor {
    pub fn push(&mut self, chunk: &str) -> String {
        self.buffer.push_str(chunk);
        if self.finished {
            return String::new();
        }

        if self.found_start.is_none() {
            if let Some(key_idx) = self.buffer.find("\"rationale\"") {
                let after_key = &self.buffer[key_idx + 11..];
                if let Some(quote_idx) = after_key.find('"') {
                    self.found_start = Some(key_idx + 11 + quote_idx + 1);
                }
            }
        }

        let mut yielded = String::new();

        if let Some(start) = self.found_start {
            let scan_start = start + self.yielded_bytes;
            if scan_start > self.buffer.len() {
                return yielded;
            }

            let to_scan = &self.buffer[scan_start..];
            let chars = to_scan.chars();

            for c in chars {
                if self.escaping {
                    self.escaping = false;
                    match c {
                        'n' => yielded.push('\n'),
                        'r' => yielded.push('\r'),
                        't' => yielded.push('\t'),
                        '"' => yielded.push('"'),
                        '\\' => yielded.push('\\'),
                        _ => {
                            yielded.push('\\');
                            yielded.push(c);
                        }
                    }
                    self.yielded_bytes += c.len_utf8();
                } else if c == '\\' {
                    self.escaping = true;
                    self.yielded_bytes += c.len_utf8();
                } else if c == '"' {
                    self.finished = true;
                    self.yielded_bytes += c.len_utf8();
                    return yielded;
                } else {
                    yielded.push(c);
                    self.yielded_bytes += c.len_utf8();
                }
            }
        }

        yielded
    }
}
