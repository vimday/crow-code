use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub trait EpistemicObserver: Send {
    fn on_step(&mut self, step: usize, max_steps: usize);
}

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
