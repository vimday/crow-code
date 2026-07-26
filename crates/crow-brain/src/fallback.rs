//! Provider fallback chain with per-provider circuit breakers.
//!
//! Wraps an ordered list of `LlmClient`s and tries them in sequence:
//! when the primary fails with a retryable error or circuit-broken state,
//! falls through to the next. After 3 consecutive failures inside a 60s
//! window, a provider is "open" and skipped for 5 minutes before being
//! probed again.
//!
//! This prevents one flaky vendor from breaking a session — Crow stays
//! online as long as ANY configured provider is healthy.

use crate::client::BrainError;
use crate::compiler::{AgentResponse, ChatMessage, LlmClient, StreamObserver, ToolStreamObserver};
use async_trait::async_trait;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;

/// Number of consecutive failures within `BREAKER_WINDOW_SECS` that trips
/// the circuit breaker open.
const BREAKER_TRIP_THRESHOLD: u32 = 3;

/// Window during which consecutive failures count toward tripping.
const BREAKER_WINDOW_SECS: i64 = 60;

/// How long a tripped breaker stays open before allowing a probe.
const BREAKER_OPEN_DURATION_SECS: i64 = 300;

/// Per-provider circuit breaker state. Lock-free; safe across threads.
struct BreakerState {
    consecutive_failures: AtomicU32,
    /// Unix-ms of last failure — used to reset the consecutive counter
    /// when failures fall outside the rolling window.
    last_failure_ms: AtomicI64,
    /// Unix-ms when the breaker was tripped open. 0 means closed.
    open_until_ms: AtomicI64,
}

impl BreakerState {
    fn new() -> Self {
        Self {
            consecutive_failures: AtomicU32::new(0),
            last_failure_ms: AtomicI64::new(0),
            open_until_ms: AtomicI64::new(0),
        }
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// True when we should skip this provider on a fresh request.
    fn is_open(&self) -> bool {
        let until = self.open_until_ms.load(Ordering::Relaxed);
        if until == 0 {
            return false;
        }
        Self::now_ms() < until
    }

    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.open_until_ms.store(0, Ordering::Relaxed);
    }

    /// Record a transient failure. Returns true when this trip just opened
    /// the breaker (caller may want to log it).
    fn record_failure(&self) -> bool {
        let now = Self::now_ms();
        let last = self.last_failure_ms.swap(now, Ordering::Relaxed);
        let in_window = last != 0 && (now - last) < (BREAKER_WINDOW_SECS * 1000);
        let count = if in_window {
            self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            self.consecutive_failures.store(1, Ordering::Relaxed);
            1
        };

        if count >= BREAKER_TRIP_THRESHOLD {
            let prev_until = self
                .open_until_ms
                .swap(now + BREAKER_OPEN_DURATION_SECS * 1000, Ordering::Relaxed);
            return prev_until == 0;
        }
        false
    }
}

/// One link in the provider chain.
struct ProviderEntry {
    label: String,
    client: Arc<dyn LlmClient>,
    breaker: BreakerState,
}

/// LlmClient that fans out across multiple providers in priority order.
pub struct FallbackLlmClient {
    providers: Vec<ProviderEntry>,
}

impl FallbackLlmClient {
    /// Build a fallback chain from `(label, client)` pairs in priority order.
    /// The first entry is the primary; later entries are tried in order
    /// when earlier ones fail or are circuit-broken.
    pub fn new(entries: Vec<(String, Arc<dyn LlmClient>)>) -> Self {
        Self {
            providers: entries
                .into_iter()
                .map(|(label, client)| ProviderEntry {
                    label,
                    client,
                    breaker: BreakerState::new(),
                })
                .collect(),
        }
    }

    /// Number of providers in the chain.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Snapshot the chain's health for diagnostics. Returns
    /// `(label, breaker_open)` for each provider.
    pub fn health_snapshot(&self) -> Vec<(String, bool)> {
        self.providers
            .iter()
            .map(|p| (p.label.clone(), p.breaker.is_open()))
            .collect()
    }

    /// Decide whether to try a provider and update breaker on result.
    fn after_attempt<T>(&self, idx: usize, result: &Result<T, BrainError>) {
        let entry = &self.providers[idx];
        match result {
            Ok(_) => entry.breaker.record_success(),
            Err(err) if err.is_retryable() => {
                let _ = entry.breaker.record_failure();
            }
            Err(_) => {
                // Non-retryable (auth, parse, config) — don't trip the breaker;
                // it would skip the provider for problems that won't recover by waiting.
            }
        }
    }
}

#[async_trait]
impl LlmClient for FallbackLlmClient {
    async fn generate(&self, messages: &[ChatMessage]) -> Result<String, BrainError> {
        if self.providers.is_empty() {
            return Err(BrainError::Config(
                "FallbackLlmClient has no providers configured".into(),
            ));
        }
        let mut last_err: Option<BrainError> = None;
        for (i, entry) in self.providers.iter().enumerate() {
            if entry.breaker.is_open() && i + 1 < self.providers.len() {
                continue;
            }
            let result = entry.client.generate(messages).await;
            self.after_attempt(i, &result);
            match result {
                Ok(text) => return Ok(text),
                Err(err) if err.is_retryable() => last_err = Some(err),
                Err(err) => return Err(err),
            }
        }
        Err(last_err
            .unwrap_or_else(|| BrainError::Config("All providers in fallback chain failed".into())))
    }

    async fn generate_with_temperature(
        &self,
        messages: &[ChatMessage],
        temperature: f64,
    ) -> Result<String, BrainError> {
        if self.providers.is_empty() {
            return Err(BrainError::Config(
                "FallbackLlmClient has no providers configured".into(),
            ));
        }
        let mut last_err: Option<BrainError> = None;
        for (i, entry) in self.providers.iter().enumerate() {
            if entry.breaker.is_open() && i + 1 < self.providers.len() {
                continue;
            }
            let result = entry
                .client
                .generate_with_temperature(messages, temperature)
                .await;
            self.after_attempt(i, &result);
            match result {
                Ok(text) => return Ok(text),
                Err(err) if err.is_retryable() => last_err = Some(err),
                Err(err) => return Err(err),
            }
        }
        Err(last_err
            .unwrap_or_else(|| BrainError::Config("All providers in fallback chain failed".into())))
    }

    async fn generate_streaming(
        &self,
        messages: &[ChatMessage],
        temperature: Option<f64>,
        observer: Option<&mut dyn StreamObserver>,
    ) -> Result<String, BrainError> {
        if self.providers.is_empty() {
            return Err(BrainError::Config(
                "FallbackLlmClient has no providers configured".into(),
            ));
        }
        // Try the primary first WITH observer (so streaming UX is preserved).
        // If the primary fails, fall through to backups WITHOUT observer —
        // user already saw the failure, and rerunning the stream into the
        // observer would produce duplicate text in the TUI.
        let mut last_err: Option<BrainError> = None;
        let mut start_idx = 0;
        for (i, entry) in self.providers.iter().enumerate() {
            if entry.breaker.is_open() && i + 1 < self.providers.len() {
                continue;
            }
            start_idx = i;
            let result = entry
                .client
                .generate_streaming(messages, temperature, observer)
                .await;
            self.after_attempt(i, &result);
            match result {
                Ok(text) => return Ok(text),
                Err(err) if err.is_retryable() => {
                    last_err = Some(err);
                    break;
                }
                Err(err) => return Err(err),
            }
        }
        for (i, entry) in self.providers.iter().enumerate().skip(start_idx + 1) {
            if entry.breaker.is_open() && i + 1 < self.providers.len() {
                continue;
            }
            let result = entry
                .client
                .generate_streaming(messages, temperature, None)
                .await;
            self.after_attempt(i, &result);
            match result {
                Ok(text) => return Ok(text),
                Err(err) if err.is_retryable() => last_err = Some(err),
                Err(err) => return Err(err),
            }
        }
        Err(last_err
            .unwrap_or_else(|| BrainError::Config("All providers in fallback chain failed".into())))
    }

    async fn generate_streaming_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        observer: Option<&mut dyn ToolStreamObserver>,
    ) -> Result<AgentResponse, BrainError> {
        if self.providers.is_empty() {
            return Err(BrainError::Config(
                "FallbackLlmClient has no providers configured".into(),
            ));
        }
        let mut last_err: Option<BrainError> = None;
        let mut start_idx = 0;
        for (i, entry) in self.providers.iter().enumerate() {
            if entry.breaker.is_open() && i + 1 < self.providers.len() {
                continue;
            }
            start_idx = i;
            let result = entry
                .client
                .generate_streaming_with_tools(messages, tools, observer)
                .await;
            self.after_attempt(i, &result);
            match result {
                Ok(resp) => return Ok(resp),
                Err(err) if err.is_retryable() => {
                    last_err = Some(err);
                    break;
                }
                Err(err) => return Err(err),
            }
        }
        for (i, entry) in self.providers.iter().enumerate().skip(start_idx + 1) {
            if entry.breaker.is_open() && i + 1 < self.providers.len() {
                continue;
            }
            let result = entry
                .client
                .generate_streaming_with_tools(messages, tools, None)
                .await;
            self.after_attempt(i, &result);
            match result {
                Ok(resp) => return Ok(resp),
                Err(err) if err.is_retryable() => last_err = Some(err),
                Err(err) => return Err(err),
            }
        }
        Err(last_err
            .unwrap_or_else(|| BrainError::Config("All providers in fallback chain failed".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Test client that succeeds or fails predictably and counts calls.
    struct StubClient {
        fail_with: Option<BrainError>,
        calls: AtomicUsize,
    }

    impl StubClient {
        fn ok() -> Self {
            Self {
                fail_with: None,
                calls: AtomicUsize::new(0),
            }
        }
        fn err(err: BrainError) -> Self {
            Self {
                fail_with: Some(err),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl LlmClient for StubClient {
        async fn generate(&self, _: &[ChatMessage]) -> Result<String, BrainError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match &self.fail_with {
                Some(err) => Err(clone_err(err)),
                None => Ok("ok".to_string()),
            }
        }
    }

    fn clone_err(err: &BrainError) -> BrainError {
        match err {
            BrainError::Config(s) => BrainError::Config(s.clone()),
            BrainError::ApiError { status, body } => BrainError::ApiError {
                status: *status,
                body: body.clone(),
            },
            BrainError::MissingField(s) => BrainError::MissingField(s.clone()),
            // Transport/Parse errors aren't easily clonable; substitute a 503.
            _ => BrainError::ApiError {
                status: 503,
                body: "stub".into(),
            },
        }
    }

    #[tokio::test]
    async fn primary_succeeds_and_fallback_unused() {
        let p1 = Arc::new(StubClient::ok());
        let p2 = Arc::new(StubClient::ok());
        let chain = FallbackLlmClient::new(vec![
            ("p1".into(), p1.clone() as Arc<dyn LlmClient>),
            ("p2".into(), p2.clone() as Arc<dyn LlmClient>),
        ]);
        let res = chain.generate(&[]).await.unwrap();
        assert_eq!(res, "ok");
        assert_eq!(p1.calls.load(Ordering::Relaxed), 1);
        assert_eq!(p2.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn fallback_used_on_retryable_error() {
        let p1 = Arc::new(StubClient::err(BrainError::ApiError {
            status: 503,
            body: "down".into(),
        }));
        let p2 = Arc::new(StubClient::ok());
        let chain = FallbackLlmClient::new(vec![
            ("p1".into(), p1.clone() as Arc<dyn LlmClient>),
            ("p2".into(), p2.clone() as Arc<dyn LlmClient>),
        ]);
        let res = chain.generate(&[]).await.unwrap();
        assert_eq!(res, "ok");
        assert_eq!(p1.calls.load(Ordering::Relaxed), 1);
        assert_eq!(p2.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn non_retryable_error_does_not_fall_through() {
        let p1 = Arc::new(StubClient::err(BrainError::Config("bad config".into())));
        let p2 = Arc::new(StubClient::ok());
        let chain = FallbackLlmClient::new(vec![
            ("p1".into(), p1.clone() as Arc<dyn LlmClient>),
            ("p2".into(), p2.clone() as Arc<dyn LlmClient>),
        ]);
        let err = chain.generate(&[]).await.err().unwrap();
        assert!(matches!(err, BrainError::Config(_)));
        assert_eq!(p1.calls.load(Ordering::Relaxed), 1);
        assert_eq!(p2.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn breaker_trips_after_threshold() {
        let p1 = Arc::new(StubClient::err(BrainError::ApiError {
            status: 503,
            body: "down".into(),
        }));
        let p2 = Arc::new(StubClient::ok());
        let chain = FallbackLlmClient::new(vec![
            ("p1".into(), p1.clone() as Arc<dyn LlmClient>),
            ("p2".into(), p2.clone() as Arc<dyn LlmClient>),
        ]);
        for _ in 0..BREAKER_TRIP_THRESHOLD {
            let _ = chain.generate(&[]).await.unwrap();
        }
        // Next call should skip p1 entirely.
        let calls_before = p1.calls.load(Ordering::Relaxed);
        let _ = chain.generate(&[]).await.unwrap();
        assert_eq!(
            p1.calls.load(Ordering::Relaxed),
            calls_before,
            "Once tripped, breaker must skip the failing provider"
        );
    }
}
