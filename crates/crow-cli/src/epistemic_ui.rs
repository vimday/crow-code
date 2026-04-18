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
}

/// A standard spinner for terminal output during epistemic loops.
pub struct SpinnerObserver {
    spinner: ProgressBar,
    message_pattern: String,
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
        Self {
            spinner,
            message_pattern: message_pattern.into(),
        }
    }

    pub fn finish(self) {
        self.spinner.finish_and_clear();
    }
}

impl EpistemicObserver for SpinnerObserver {
    fn on_step(&mut self, step: usize, max: usize) {
        let text = self
            .message_pattern
            .replace("{step}", &step.to_string())
            .replace("{max}", &max.to_string());
        self.spinner.set_message(text);
    }
}

// Allow passing closures directly
impl<F> EpistemicObserver for F
where
    F: FnMut(usize, usize) + Send,
{
    fn on_step(&mut self, step: usize, max: usize) {
        (self)(step, max)
    }
}
