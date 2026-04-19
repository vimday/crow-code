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
        }
    }
    
    fn stop_spinner(&mut self) {
        if let Some(sp) = self.spinner.take() {
            sp.finish();
            
            // If we had streamed markdown, ensure it's fully flushed when spinning stops
            if let Some(final_md) = self.markdown_state.flush(&self.renderer) {
                print!("{}", final_md);
            }
        }
    }
}

impl EventHandler for CliEventHandler {
    fn handle_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Thinking(step, max) => {
                self.stop_spinner();
                self.stream_char_count = 0;
                self.rationale_processor = RationaleStreamProcessor::default();
                self.markdown_state = crate::render::MarkdownStreamState::default();
                
                self.spinner = Some(crate::epistemic_ui::SpinnerObserver::new(format!(
                    "🧠 Epistemic Step {}/{} — Synthesizing...",
                    step, max
                )));
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
                    
                    if let Some(rendered) = self.markdown_state.push(&self.renderer, &extracted_text) {
                        use std::io::Write;
                        print!("{}", rendered);
                        let _ = std::io::stdout().flush();
                    }
                } else if let Some(ref mut sp) = self.spinner {
                    let kb = self.stream_char_count as f64 / 1024.0;
                    sp.set_status(format!("{:.1}K received", kb));
                }
            }
            AgentEvent::ActionStart(desc) => {
                self.stop_spinner();
                println!("  🚀 {}", desc);
            }
            AgentEvent::ActionComplete(desc) => {
                println!("  ✅ {}", desc);
            }
            AgentEvent::ReadFiles(paths) => {
                self.stop_spinner();
                let display = if paths.len() <= 3 {
                    paths.join(", ")
                } else {
                    format!("{}, ... ({} files)", paths[..2].join(", "), paths.len())
                };
                println!("  📖 Reading: {}", display);
            }
            AgentEvent::ReconStart(desc) => {
                self.stop_spinner();
                println!("  🔍 Recon: {}", desc);
            }
            AgentEvent::DelegateStart(task) => {
                self.stop_spinner();
                println!("  🤖 Delegating: {}", task);
            }
            AgentEvent::PlanSubmitted(plan) => {
                self.stop_spinner();
                if plan.operations.is_empty() {
                    // Already streamed out!
                } else {
                    println!("  📋 Plan submitted: {} operations, confidence: {:?}",
                        plan.operations.len(), plan.confidence);
                }
            }
            AgentEvent::CruciblePreflight(msg) => {
                self.stop_spinner();
                println!("  🛡️  Preflight: {}", msg);
            }
            AgentEvent::Log(msg) => {
                println!("{}", msg);
            }
            AgentEvent::Error(err) => {
                self.stop_spinner();
                eprintln!("  ❌ Error: {}", err);
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
        if self.finished { return String::new(); }
        
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
