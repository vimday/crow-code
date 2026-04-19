//! Epistemic loop UI dispatchers.
//!
//! Provides the `EpistemicObserver` trait to abstract progress reporting
//! from the underlying MCTS and Serial engines.

use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// An abstraction over progress reporting for internal epistemic loops.
///
/// **Responsibilities**: Decouple the core logic execution (compilation, semantic search, reasoning states)
/// from the standard output and console UI components. This ensures that the engine can run iteratively
/// without needing hardcoded dependencies on specific terminal drawing crates like `indicatif`.
///
/// **Usage Scenarios**:
/// - `SpinnerObserver` can be injected into interactive/serial CLI operations to show real-time progress.
/// - `SilentObserver` can be injected during high-velocity asynchronous multi-branch explorations (MCTS)
///   to prevent terminal spamming or concurrent drawing collisions across threaded branch evaluators.
///
/// **Implementation Constraints**:
/// - Must implement `Send` so progress tracking can be safely moved into `tokio` multi-threaded executor boundaries.
/// - Operations inside `on_step` must be non-blocking to prevent UI feedback loops from hanging the cognitive request.
pub trait EpistemicObserver: Send {
    /// Called per epistemic compiler step.
    fn on_step(&mut self, step: usize, max_steps: usize);
    /// Called when an SSE token arrives from the language model generator.
    fn on_stream_chunk(&mut self, chunk: &str);
}

/// A null-object pattern implementation of `EpistemicObserver`.
///
/// **Responsibilities**: Discards all status updates seamlessly.
///
/// **Usage Scenarios**: Utilized by parallel `MCTS` exploration branches where spawning multiple
/// concurrent overlapping spinners would completely corrupt the terminal multiplexer and create noise.
pub struct SilentObserver;
impl EpistemicObserver for SilentObserver {
    fn on_step(&mut self, _step: usize, _max: usize) {}
    fn on_stream_chunk(&mut self, _chunk: &str) {}
}

impl crow_brain::compiler::StreamObserver for SilentObserver {
    fn on_chunk(&mut self, _chunk: &str) {}
}

/// A standard spinner for terminal output during epistemic loops.
pub struct SpinnerObserver {
    spinner: ProgressBar,
    message_pattern: String,
    stream_buffer: String,
}

impl SpinnerObserver {
    pub fn new(message_pattern: impl Into<String>) -> Self {
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::default_spinner()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
                .template("{spinner:.cyan} {msg}")
                .unwrap(),
        );
        if console::Term::stdout().is_term() && std::env::var("CI").is_err() {
            spinner.enable_steady_tick(Duration::from_millis(100));
        }
        let msg = message_pattern.into();
        spinner.set_message(msg.clone());
        Self {
            spinner,
            message_pattern: msg,
            stream_buffer: String::with_capacity(256),
        }
    }

    pub fn finish(self) {
        self.spinner.finish_and_clear();
    }

    /// Update the spinner's display with a clean status suffix.
    pub fn set_status(&mut self, status: String) {
        self.spinner.set_message(format!("{} — {}", self.message_pattern, status));
    }
}

impl EpistemicObserver for SpinnerObserver {
    fn on_step(&mut self, step: usize, max: usize) {
        self.stream_buffer.clear(); // Reset text when a new step starts
        let msg = self.message_pattern.replace("{}", &step.to_string());
        self.spinner
            .set_message(format!("{} (of {} max)", msg, max));
    }

    fn on_stream_chunk(&mut self, chunk: &str) {
        self.stream_buffer.push_str(chunk);
        
        let cleaned = self.stream_buffer.replace('\n', " ");
        let display_len = 60;
        let suffix = if cleaned.chars().count() > display_len {
            let start = cleaned.chars().count() - display_len;
            let substr: String = cleaned.chars().skip(start).collect();
            format!("...{}", substr)
        } else {
            cleaned
        };

        // We embed the real-time reasoning trace directly into the spinner output
        self.spinner.set_message(format!("{} ⚡ {}", 
            self.message_pattern.replace("{}", "?"), 
            suffix
        ));
    }
}

impl crow_brain::compiler::StreamObserver for SpinnerObserver {
    fn on_chunk(&mut self, chunk: &str) {
        self.on_stream_chunk(chunk);
    }
}

// Allow passing closures directly
impl<F> EpistemicObserver for F
where
    F: FnMut(usize, usize) + Send,
{
    fn on_step(&mut self, step: usize, max: usize) {
        self(step, max);
    }
    fn on_stream_chunk(&mut self, _chunk: &str) {}
}
