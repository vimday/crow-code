//! Smooth streaming animation controller (CommitTick pattern).
//!
//! Buffers rendered markdown lines from the agent's streaming output and
//! drains them one-at-a-time on each TUI tick, creating a smooth typewriter
//! effect instead of dumping an entire wall of text at once.
//!
//! Inspired by codex's `CommitTickScope` + `AdaptiveChunkingPolicy`.

use super::state::{Cell, CellKind};

/// Controls how many lines are committed per tick.
///
/// Starts at 1 line per tick for smooth animation.
/// If the pending queue grows beyond `BACKLOG_THRESHOLD`, the policy
/// switches to draining multiple lines per tick to catch up.
const LINES_PER_TICK_BASE: usize = 1;
const BACKLOG_THRESHOLD: usize = 20;
const LINES_PER_TICK_CATCHUP: usize = 5;

/// Manages the lifecycle of a streaming agent response.
///
/// The controller sits between the raw `StreamChunk` events and the
/// history pane. Instead of pushing lines immediately, it buffers
/// them and releases them at a controlled rate via `drain_tick()`.
#[derive(Debug, Default)]
pub struct StreamController {
    /// Lines waiting to be committed to history.
    pending: Vec<Cell>,
    /// Whether the stream is actively receiving chunks.
    active: bool,
    /// Accumulated raw text for the current streaming markdown pass.
    stream_state: crate::render::MarkdownStreamState,
}

impl StreamController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a new streaming session. Clears any residual state.
    pub fn start(&mut self) {
        self.pending.clear();
        self.active = true;
        self.stream_state.clear();
    }

    /// Returns true if the controller is actively buffering a stream.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Returns the number of lines waiting to be committed.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Push a raw text delta from `AgentEvent::StreamChunk`.
    ///
    /// The delta is fed through the newline-gated markdown renderer.
    /// Completed lines are buffered in `pending` for tick-driven drain.
    pub fn push_chunk(&mut self, chunk: &str) {
        let renderer = crate::render::TerminalRenderer::new();
        if let Some(rendered) = self.stream_state.push(&renderer, chunk) {
            for line in rendered.lines() {
                self.pending.push(Cell {
                    kind: CellKind::AgentMessage,
                    payload: line.to_string(),
                });
            }
        }
    }

    /// Push a final markdown block (from `AgentEvent::Markdown`).
    ///
    /// Flushes any remaining stream buffer, then renders and enqueues
    /// the final markdown content.
    pub fn push_markdown(&mut self, md: &str) {
        let renderer = crate::render::TerminalRenderer::new();

        // Flush any remaining stream buffer
        if let Some(flushed) = self.stream_state.flush(&renderer) {
            for line in flushed.lines() {
                self.pending.push(Cell {
                    kind: CellKind::AgentMessage,
                    payload: line.to_string(),
                });
            }
        }

        // Render the final markdown block
        let rendered = renderer.render_markdown(md);
        for line in rendered.lines() {
            self.pending.push(Cell {
                kind: CellKind::AgentMessage,
                payload: line.to_string(),
            });
        }
    }

    /// Finalize the stream. Flushes any remaining buffered content
    /// and marks the controller as inactive.
    ///
    /// After calling this, `drain_tick()` will continue draining
    /// pending lines but no new chunks will be accepted.
    pub fn finish(&mut self) {
        let renderer = crate::render::TerminalRenderer::new();
        if let Some(flushed) = self.stream_state.flush(&renderer) {
            for line in flushed.lines() {
                self.pending.push(Cell {
                    kind: CellKind::AgentMessage,
                    payload: line.to_string(),
                });
            }
        }
        self.active = false;
    }

    /// Drain lines on a tick boundary (called every 120ms from the TUI tick).
    ///
    /// Returns a batch of cells to commit to history. The batch size
    /// adapts based on backlog pressure:
    /// - Normal: 1 line per tick (smooth typewriter)
    /// - Backlogged (>20 pending): 5 lines per tick (catch-up mode)
    /// - Stream ended + pending: flush all remaining immediately
    pub fn drain_tick(&mut self) -> Vec<Cell> {
        if self.pending.is_empty() {
            return Vec::new();
        }

        let batch_size = if !self.active {
            // Stream ended — flush everything remaining immediately
            self.pending.len()
        } else if self.pending.len() > BACKLOG_THRESHOLD {
            // Backlog pressure — drain faster to catch up
            LINES_PER_TICK_CATCHUP.min(self.pending.len())
        } else {
            // Normal smooth animation
            LINES_PER_TICK_BASE.min(self.pending.len())
        };

        self.pending.drain(..batch_size).collect()
    }

    /// Force-drain all pending content immediately. Used when the turn
    /// is interrupted or on error recovery.
    pub fn drain_all(&mut self) -> Vec<Cell> {
        self.active = false;
        self.pending.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_drain_one_per_tick() {
        let mut ctrl = StreamController::new();
        ctrl.start();

        // Simulate 5 buffered lines
        for i in 0..5 {
            ctrl.pending.push(Cell {
                kind: CellKind::AgentMessage,
                payload: format!("line {i}"),
            });
        }

        // Each tick should drain exactly 1 line
        assert_eq!(ctrl.drain_tick().len(), 1);
        assert_eq!(ctrl.pending_count(), 4);
        assert_eq!(ctrl.drain_tick().len(), 1);
        assert_eq!(ctrl.pending_count(), 3);
    }

    #[test]
    fn catchup_on_backlog() {
        let mut ctrl = StreamController::new();
        ctrl.start();

        // Simulate heavy backlog (25 lines)
        for i in 0..25 {
            ctrl.pending.push(Cell {
                kind: CellKind::AgentMessage,
                payload: format!("line {i}"),
            });
        }

        // Should drain LINES_PER_TICK_CATCHUP lines
        assert_eq!(ctrl.drain_tick().len(), LINES_PER_TICK_CATCHUP);
    }

    #[test]
    fn flush_all_on_finish() {
        let mut ctrl = StreamController::new();
        ctrl.start();

        for i in 0..10 {
            ctrl.pending.push(Cell {
                kind: CellKind::AgentMessage,
                payload: format!("line {i}"),
            });
        }

        ctrl.finish();

        // After finish, drain_tick should flush everything
        let drained = ctrl.drain_tick();
        assert_eq!(drained.len(), 10);
        assert_eq!(ctrl.pending_count(), 0);
    }

    #[test]
    fn drain_all_force_flushes() {
        let mut ctrl = StreamController::new();
        ctrl.start();

        for i in 0..8 {
            ctrl.pending.push(Cell {
                kind: CellKind::AgentMessage,
                payload: format!("line {i}"),
            });
        }

        let drained = ctrl.drain_all();
        assert_eq!(drained.len(), 8);
        assert!(!ctrl.is_active());
    }
}
