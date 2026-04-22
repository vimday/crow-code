/// Resettable cancellation token using arc-swap + `CancellationToken`.
///
/// Design (aligned with yomi's `CancelToken`):
/// - Clone shares the same `ArcSwap` (shared cancellation state)
/// - `cancel()` / `reset_if_cancelled()` operate atomically via `ArcSwap`
/// - `runtime_token()` snapshots the current token for `tokio::select!` use
///
/// This enables safe interruption of agent turns: the frontend cancels via
/// `cancel()`, the epistemic loop checks via `is_cancelled()`, and after
/// the turn completes, `reset_if_cancelled()` prepares for the next turn
/// without affecting stale listeners on the old token.
#[derive(Clone)]
pub struct CancellationToken {
    inner: std::sync::Arc<arc_swap::ArcSwap<tokio_util::sync::CancellationToken>>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                tokio_util::sync::CancellationToken::new(),
            )),
        }
    }

    /// Mark the current token as cancelled.
    pub fn cancel(&self) {
        self.inner.load().cancel();
    }

    /// Check if the current token is cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.load().is_cancelled()
    }

    /// If the token was cancelled, swap it out for a fresh one.
    /// This is typically called at the beginning/end of a turn lifecycle.
    pub fn reset_if_cancelled(&self) {
        let current = self.inner.load();
        if current.is_cancelled() {
            self.inner.store(std::sync::Arc::new(
                tokio_util::sync::CancellationToken::new(),
            ));
        }
    }

    /// Unconditionally swap in a new token.
    pub fn force_reset(&self) {
        self.inner.store(std::sync::Arc::new(
            tokio_util::sync::CancellationToken::new(),
        ));
    }

    /// Get a snapshot of the underlying tokio cancellation token.
    pub fn runtime_token(&self) -> tokio_util::sync::CancellationToken {
        (**self.inner.load()).clone()
    }
}
