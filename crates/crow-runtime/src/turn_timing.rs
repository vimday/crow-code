//! Turn timing state machine (Codex pattern).
//!
//! Tracks fine-grained timing metrics for agent turns:
//! - Time to First Token (TTFT): how long until the first LLM token arrives
//! - Time to First Message (TTFM): how long until the first complete message
//! - Total turn duration
//!
//! All state is behind a `Mutex` so metrics can be recorded from any
//! async context without requiring `&mut self`.

use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Async-safe turn timing state.
///
/// Create one per turn, call `mark_turn_started()` at the beginning,
/// `record_first_token()` when the first LLM token streams in, and
/// `record_first_message()` when the first complete message is available.
#[derive(Debug, Default)]
pub struct TurnTimingState {
    inner: Mutex<TurnTimingInner>,
}

#[derive(Debug, Default)]
struct TurnTimingInner {
    started_at: Option<Instant>,
    first_token_at: Option<Instant>,
    first_message_at: Option<Instant>,
}

/// Snapshot of timing metrics for a completed turn.
#[derive(Debug, Clone)]
pub struct TurnTimingSnapshot {
    /// Total wall-clock time of the turn.
    pub total_ms: u64,
    /// Time from turn start to first LLM token (TTFT).
    pub ttft_ms: Option<u64>,
    /// Time from turn start to first complete message (TTFM).
    pub ttfm_ms: Option<u64>,
}

impl TurnTimingState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the turn as started. Resets all timing state.
    pub async fn mark_turn_started(&self) {
        let mut state = self.inner.lock().await;
        state.started_at = Some(Instant::now());
        state.first_token_at = None;
        state.first_message_at = None;
    }

    /// Record the arrival of the first LLM token.
    /// Returns the TTFT duration if this is the first call (idempotent).
    pub async fn record_first_token(&self) -> Option<Duration> {
        let mut state = self.inner.lock().await;
        if state.first_token_at.is_some() {
            return None;
        }
        let started = state.started_at?;
        let now = Instant::now();
        state.first_token_at = Some(now);
        Some(now.duration_since(started))
    }

    /// Record the arrival of the first complete message.
    /// Returns the TTFM duration if this is the first call (idempotent).
    pub async fn record_first_message(&self) -> Option<Duration> {
        let mut state = self.inner.lock().await;
        if state.first_message_at.is_some() {
            return None;
        }
        let started = state.started_at?;
        let now = Instant::now();
        state.first_message_at = Some(now);
        Some(now.duration_since(started))
    }

    /// Get the TTFT if it has been recorded.
    pub async fn ttft(&self) -> Option<Duration> {
        let state = self.inner.lock().await;
        let started = state.started_at?;
        let first_token = state.first_token_at?;
        Some(first_token.duration_since(started))
    }

    /// Capture a complete snapshot of timing metrics.
    /// Call this when the turn completes.
    pub async fn snapshot(&self) -> Option<TurnTimingSnapshot> {
        let state = self.inner.lock().await;
        let started = state.started_at?;
        let total = Instant::now().duration_since(started);

        Some(TurnTimingSnapshot {
            total_ms: total.as_millis() as u64,
            ttft_ms: state
                .first_token_at
                .map(|ft| ft.duration_since(started).as_millis() as u64),
            ttfm_ms: state
                .first_message_at
                .map(|fm| fm.duration_since(started).as_millis() as u64),
        })
    }
}

/// Exponential backoff with jitter (Codex pattern).
///
/// Returns a delay duration for the given retry attempt (1-indexed).
/// Uses 200ms base delay × 2^(attempt-1) with ±10% jitter to
/// decorrelate concurrent retry storms.
///
/// Jitter is derived from `Instant::now()` nanoseconds to avoid
/// pulling in the `rand` crate as a dependency.
pub fn backoff_with_jitter(attempt: u32) -> Duration {
    const INITIAL_DELAY_MS: u64 = 200;
    const BACKOFF_FACTOR: f64 = 2.0;

    let exp = BACKOFF_FACTOR.powi(attempt.saturating_sub(1) as i32);
    let base = (INITIAL_DELAY_MS as f64 * exp) as u64;
    // Lightweight jitter: use the nanosecond component of the system clock
    // to produce a multiplier in [0.9, 1.1).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter = 0.9 + (nanos % 200) as f64 / 1000.0; // [0.9, 1.1)
    Duration::from_millis((base as f64 * jitter) as u64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_timing_lifecycle() {
        let timing = TurnTimingState::new();
        timing.mark_turn_started().await;

        // First token should return Some
        let ttft = timing.record_first_token().await;
        assert!(ttft.is_some());

        // Second call is idempotent
        assert!(timing.record_first_token().await.is_none());

        // First message
        let ttfm = timing.record_first_message().await;
        assert!(ttfm.is_some());
        assert!(timing.record_first_message().await.is_none());

        // Snapshot
        let snap = timing.snapshot().await;
        assert!(snap.is_some(), "should have snapshot after started turn");
        if let Some(snap) = snap {
            assert!(snap.ttft_ms.is_some());
            assert!(snap.ttfm_ms.is_some());
        }
    }

    #[tokio::test]
    async fn test_timing_no_start() {
        let timing = TurnTimingState::new();
        // Without mark_turn_started, everything returns None
        assert!(timing.record_first_token().await.is_none());
        assert!(timing.snapshot().await.is_none());
    }

    #[test]
    fn test_backoff_with_jitter() {
        let d1 = backoff_with_jitter(1);
        let d2 = backoff_with_jitter(2);
        let d3 = backoff_with_jitter(3);
        // Each delay should roughly double
        assert!(d2 > d1);
        assert!(d3 > d2);
        // Should be in reasonable range
        assert!(d1.as_millis() < 300);
        assert!(d3.as_millis() < 2000);
    }
}
