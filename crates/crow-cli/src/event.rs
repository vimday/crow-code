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
}

impl Default for CliEventHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl CliEventHandler {
    pub fn new() -> Self {
        Self { spinner: None, stream_char_count: 0 }
    }
    
    fn stop_spinner(&mut self) {
        if let Some(sp) = self.spinner.take() {
            sp.finish();
        }
    }
}

impl EventHandler for CliEventHandler {
    fn handle_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Thinking(step, max) => {
                self.stop_spinner();
                self.stream_char_count = 0;
                self.spinner = Some(crate::epistemic_ui::SpinnerObserver::new(format!(
                    "🧠 Epistemic Step {}/{} — Synthesizing...",
                    step, max
                )));
            }
            AgentEvent::StreamChunk(chunk) => {
                self.stream_char_count += chunk.len();
                if let Some(ref mut sp) = self.spinner {
                    // Show character progress rather than truncated JSON fragments.
                    // The stream chunks during tool calls are raw JSON that looks
                    // like garbage when sliced into a spinner suffix.
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
                    println!("  💬 Agent responded (conversational, no code changes)");
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
