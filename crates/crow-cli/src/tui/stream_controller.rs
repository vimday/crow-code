//! Smooth streaming animation controller (CommitTick pattern).
//!
//! Buffers rendered markdown lines from the agent's streaming output and
//! drains them one-at-a-time on each TUI tick, creating a smooth typewriter
//! effect instead of dumping an entire wall of text at once.
//!
//! Inspired by codex's `CommitTickScope` + `AdaptiveChunkingPolicy`.
//!
//! ## Adaptive Chunking Policy
//!
//! Uses a two-gear hysteresis system matching Codex's `chunking.rs`:
//!
//! - **Smooth mode**: drain 1 line per commit tick (baseline)
//! - **CatchUp mode**: drain all queued lines per tick (pressure relief)
//!
//! Transition rules use hysteresis to avoid gear-flapping:
//! - Enter CatchUp: depth ≥ 8 OR oldest age ≥ 120ms
//! - Exit CatchUp: depth ≤ 2 AND age ≤ 40ms, held for 250ms
//! - Re-entry hold: 250ms cooldown after exit (bypassed for severe backlog ≥ 64)

/// Internal cell type for stream controller buffering.
/// This is a local equivalent of the old `Cell` — it never leaves this module.
#[derive(Debug, Clone)]
pub struct StreamCell {
    pub payload: String,
}

use std::time::{Duration, Instant};

// ─── Adaptive Chunking Constants (Codex parity) ────────────────────

/// Queue-depth threshold that allows entering catch-up mode.
const ENTER_QUEUE_DEPTH_LINES: usize = 8;
/// Oldest-line age threshold that allows entering catch-up mode.
const ENTER_OLDEST_AGE: Duration = Duration::from_millis(120);
/// Queue-depth threshold for evaluating catch-up exit.
const EXIT_QUEUE_DEPTH_LINES: usize = 2;
/// Oldest-line age threshold for evaluating catch-up exit.
const EXIT_OLDEST_AGE: Duration = Duration::from_millis(40);
/// Minimum duration pressure must stay below exit thresholds to leave catch-up.
const EXIT_HOLD: Duration = Duration::from_millis(250);
/// Cooldown window after a catch-up exit that suppresses immediate re-entry.
const REENTER_CATCH_UP_HOLD: Duration = Duration::from_millis(250);
/// Queue-depth cutoff that marks backlog as severe (bypasses re-entry hold).
const SEVERE_QUEUE_DEPTH_LINES: usize = 64;
/// Oldest-line age cutoff that marks backlog as severe.
const SEVERE_OLDEST_AGE: Duration = Duration::from_millis(300);

// ─── Chunking Mode ─────────────────────────────────────────────────

/// Adaptive chunking mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChunkingMode {
    /// Drain one line per baseline commit tick.
    #[default]
    Smooth,
    /// Drain all queued lines per tick to relieve backlog pressure.
    CatchUp,
}

// ─── Stream Controller ─────────────────────────────────────────────

/// Manages the lifecycle of a streaming agent response.
///
/// The controller sits between the raw `StreamChunk` events and the
/// history pane. Instead of pushing lines immediately, it buffers
/// them and releases them at a controlled rate via `drain_tick()`.
#[derive(Debug, Default)]
pub struct StreamController {
    /// Lines waiting to be committed to history.
    pending: Vec<StreamCell>,
    /// Whether the stream is actively receiving chunks.
    active: bool,
    /// Accumulated raw text for the current streaming markdown pass.
    stream_state: crate::render::MarkdownStreamState,

    // ── Adaptive Chunking State (Codex hysteresis) ──────────────
    /// Current chunking mode.
    mode: ChunkingMode,
    /// When each pending line was enqueued (for age-based pressure).
    enqueue_times: Vec<Instant>,
    /// Timestamp when queue pressure first dropped below exit thresholds.
    below_exit_since: Option<Instant>,
    /// Timestamp of the last catch-up exit (for re-entry hold).
    last_catch_up_exit: Option<Instant>,

    // ── Two-Region Streaming State ──────────────────────────────
    /// Lines that have been committed to the stable region (finalized).
    stable_lines: Vec<String>,
    /// Lines currently held back because they look like a partial table.
    table_holdback: Vec<String>,
    /// Whether we're currently inside a potential table sequence.
    in_table_sequence: bool,
}

impl StreamController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a new streaming session. Clears any residual state.
    pub fn start(&mut self) {
        self.pending.clear();
        self.enqueue_times.clear();
        self.active = true;
        self.stream_state.clear();
        self.mode = ChunkingMode::Smooth;
        self.below_exit_since = None;
        self.last_catch_up_exit = None;
        self.stable_lines.clear();
        self.table_holdback.clear();
        self.in_table_sequence = false;
    }

    /// Returns true if the controller is actively buffering a stream.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Returns the number of lines waiting to be committed.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Returns the current chunking mode.
    pub fn mode(&self) -> ChunkingMode {
        self.mode
    }

    /// Push a raw text delta from `AgentEvent::StreamChunk`.
    ///
    /// The delta is fed through the newline-gated markdown renderer.
    /// Completed lines are buffered in `pending` for tick-driven drain.
    /// Lines that look like markdown pipe-table rows are held back until
    /// the table is complete (a non-pipe line arrives or the stream ends).
    pub fn push_chunk(&mut self, chunk: &str) {
        let renderer = crate::render::TerminalRenderer::new();
        if let Some(rendered) = self.stream_state.push(&renderer, chunk) {
            let now = Instant::now();
            for line in rendered.lines() {
                let trimmed = line.trim_start();
                if trimmed.starts_with('|') {
                    // Pipe-table row detected — hold it back.
                    self.table_holdback.push(line.to_string());
                    self.in_table_sequence = true;
                } else {
                    // Non-pipe line: flush any accumulated table holdback
                    // first, then enqueue this line normally.
                    if self.in_table_sequence {
                        for held in self.table_holdback.drain(..) {
                            self.pending.push(StreamCell { payload: held });
                            self.enqueue_times.push(now);
                        }
                        self.in_table_sequence = false;
                    }
                    self.pending.push(StreamCell {
                        payload: line.to_string(),
                    });
                    self.enqueue_times.push(now);
                }
            }
        }
    }

    /// Push a final markdown block (from `AgentEvent::Markdown`).
    ///
    /// Flushes any remaining stream buffer, then renders and enqueues
    /// the final markdown content.
    pub fn push_markdown(&mut self, md: &str) {
        let renderer = crate::render::TerminalRenderer::new();
        let now = Instant::now();

        // Flush any remaining stream buffer
        if let Some(flushed) = self.stream_state.flush(&renderer) {
            for line in flushed.lines() {
                self.pending.push(StreamCell {
                    payload: line.to_string(),
                });
                self.enqueue_times.push(now);
            }
        }

        // Render the final markdown block
        let rendered = renderer.render_markdown(md);
        for line in rendered.lines() {
            self.pending.push(StreamCell {
                payload: line.to_string(),
            });
            self.enqueue_times.push(now);
        }
    }

    /// Finalize the stream. Flushes any remaining buffered content
    /// and marks the controller as inactive.
    ///
    /// After calling this, `drain_tick()` will continue draining
    /// pending lines but no new chunks will be accepted.
    pub fn finish(&mut self) {
        let renderer = crate::render::TerminalRenderer::new();
        let now = Instant::now();
        if let Some(flushed) = self.stream_state.flush(&renderer) {
            for line in flushed.lines() {
                self.pending.push(StreamCell {
                    payload: line.to_string(),
                });
                self.enqueue_times.push(now);
            }
        }
        // Flush any held-back table lines on stream end.
        self.flush_holdback();
        self.active = false;
    }

    /// Returns the age of the oldest queued line.
    fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.enqueue_times
            .first()
            .map(|t| now.saturating_duration_since(*t))
    }

    /// Drain lines on a tick boundary (called every 120ms from the TUI tick).
    ///
    /// Uses Codex-style adaptive chunking with hysteresis:
    /// - Smooth mode: 1 line per tick
    /// - CatchUp mode: all queued lines per tick
    /// - Stream ended + pending: flush all remaining immediately
    pub fn drain_tick(&mut self) -> Vec<StreamCell> {
        if self.pending.is_empty() {
            // Reset to smooth when queue is empty
            self.note_catch_up_exit();
            self.mode = ChunkingMode::Smooth;
            self.below_exit_since = None;
            return Vec::new();
        }

        // If stream ended, flush everything immediately
        if !self.active {
            self.enqueue_times.clear();
            let drained: Vec<StreamCell> = self.pending.drain(..).collect();
            // Commit everything we just drained to stable.
            for cell in &drained {
                self.stable_lines.push(cell.payload.clone());
            }
            return drained;
        }

        let now = Instant::now();
        let queued_lines = self.pending.len();
        let oldest_age = self.oldest_queued_age(now);

        // ── Adaptive mode transitions ───────────────────────────────
        match self.mode {
            ChunkingMode::Smooth => {
                if self.should_enter_catch_up(queued_lines, oldest_age, now) {
                    self.mode = ChunkingMode::CatchUp;
                    self.below_exit_since = None;
                    self.last_catch_up_exit = None;
                }
            }
            ChunkingMode::CatchUp => {
                self.maybe_exit_catch_up(queued_lines, oldest_age, now);
            }
        }

        // ── Drain based on current mode ─────────────────────────────
        let batch_size = match self.mode {
            ChunkingMode::Smooth => 1.min(self.pending.len()),
            ChunkingMode::CatchUp => self.pending.len(),
        };

        self.enqueue_times.drain(..batch_size);
        let drained: Vec<StreamCell> = self.pending.drain(..batch_size).collect();
        // Commit drained lines to the stable region.
        self.commit_to_stable(&drained);
        drained
    }

    /// Force-drain all pending content immediately. Used when the turn
    /// is interrupted or on error recovery.
    pub fn drain_all(&mut self) -> Vec<StreamCell> {
        self.active = false;
        self.flush_holdback();
        self.enqueue_times.clear();
        let drained: Vec<StreamCell> = self.pending.drain(..).collect();
        self.commit_to_stable(&drained);
        drained
    }

    // ── Adaptive Chunking Hysteresis (Codex parity) ─────────────

    fn should_enter_catch_up(
        &self,
        queued_lines: usize,
        oldest_age: Option<Duration>,
        now: Instant,
    ) -> bool {
        let pressure = queued_lines >= ENTER_QUEUE_DEPTH_LINES
            || oldest_age.is_some_and(|age| age >= ENTER_OLDEST_AGE);

        if !pressure {
            return false;
        }

        // Check re-entry hold (unless severe backlog)
        if self.reentry_hold_active(now) && !self.is_severe_backlog(queued_lines, oldest_age) {
            return false;
        }

        true
    }

    fn maybe_exit_catch_up(
        &mut self,
        queued_lines: usize,
        oldest_age: Option<Duration>,
        now: Instant,
    ) {
        let below_exit = queued_lines <= EXIT_QUEUE_DEPTH_LINES
            && oldest_age.is_some_and(|age| age <= EXIT_OLDEST_AGE);

        if !below_exit {
            self.below_exit_since = None;
            return;
        }

        match self.below_exit_since {
            Some(since) if now.saturating_duration_since(since) >= EXIT_HOLD => {
                self.mode = ChunkingMode::Smooth;
                self.below_exit_since = None;
                self.last_catch_up_exit = Some(now);
            }
            Some(_) => {}
            None => {
                self.below_exit_since = Some(now);
            }
        }
    }

    fn note_catch_up_exit(&mut self) {
        if self.mode == ChunkingMode::CatchUp {
            self.last_catch_up_exit = Some(Instant::now());
        }
    }

    fn reentry_hold_active(&self, now: Instant) -> bool {
        self.last_catch_up_exit
            .is_some_and(|exit| now.saturating_duration_since(exit) < REENTER_CATCH_UP_HOLD)
    }

    fn is_severe_backlog(&self, queued_lines: usize, oldest_age: Option<Duration>) -> bool {
        queued_lines >= SEVERE_QUEUE_DEPTH_LINES
            || oldest_age.is_some_and(|age| age >= SEVERE_OLDEST_AGE)
    }

    // ── Two-Region Streaming ────────────────────────────────────

    /// Move drained lines into the stable region (finalized scrollback).
    fn commit_to_stable(&mut self, cells: &[StreamCell]) {
        for cell in cells {
            self.stable_lines.push(cell.payload.clone());
        }
    }

    /// Force the table holdback buffer to commit. Called when the stream
    /// ends so that any trailing table rows are not lost.
    pub fn flush_holdback(&mut self) {
        if self.table_holdback.is_empty() {
            return;
        }
        let now = Instant::now();
        for held in self.table_holdback.drain(..) {
            self.pending.push(StreamCell { payload: held });
            self.enqueue_times.push(now);
        }
        self.in_table_sequence = false;
    }

    /// Returns the number of lines in the stable (finalized) region.
    pub fn stable_line_count(&self) -> usize {
        self.stable_lines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_drain_one_per_tick() {
        let mut ctrl = StreamController::new();
        ctrl.start();

        let now = Instant::now();
        // Simulate 5 buffered lines
        for i in 0..5 {
            ctrl.pending.push(StreamCell {
                payload: format!("line {i}"),
            });
            ctrl.enqueue_times.push(now);
        }

        // Each tick should drain exactly 1 line
        assert_eq!(ctrl.drain_tick().len(), 1);
        assert_eq!(ctrl.pending_count(), 4);
        assert_eq!(ctrl.drain_tick().len(), 1);
        assert_eq!(ctrl.pending_count(), 3);
    }

    #[test]
    fn catchup_on_depth_threshold() {
        let mut ctrl = StreamController::new();
        ctrl.start();

        let now = Instant::now();
        // Simulate 10 lines (above ENTER_QUEUE_DEPTH_LINES = 8)
        for i in 0..10 {
            ctrl.pending.push(StreamCell {
                payload: format!("line {i}"),
            });
            ctrl.enqueue_times.push(now);
        }

        // Should enter CatchUp and drain all lines
        let drained = ctrl.drain_tick();
        assert_eq!(drained.len(), 10);
        assert_eq!(ctrl.mode(), ChunkingMode::CatchUp);
    }

    #[test]
    fn flush_all_on_finish() {
        let mut ctrl = StreamController::new();
        ctrl.start();

        let now = Instant::now();
        for i in 0..10 {
            ctrl.pending.push(StreamCell {
                payload: format!("line {i}"),
            });
            ctrl.enqueue_times.push(now);
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

        let now = Instant::now();
        for i in 0..8 {
            ctrl.pending.push(StreamCell {
                payload: format!("line {i}"),
            });
            ctrl.enqueue_times.push(now);
        }

        let drained = ctrl.drain_all();
        assert_eq!(drained.len(), 8);
        assert!(!ctrl.is_active());
    }

    #[test]
    fn smooth_mode_is_default() {
        let ctrl = StreamController::new();
        assert_eq!(ctrl.mode(), ChunkingMode::Smooth);
    }
}
