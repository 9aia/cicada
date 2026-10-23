//! `RetryHttp` — exponential backoff + jitter around any [`Http`]
//! (ADR-0001 §4: retries live in the adapter layer, below the
//! `CalendarProvider` port, so use cases never think about rate
//! limits).
//!
//! A *request* is retried while it fails transiently:
//!
//! - `Err(CicadaError::Transient { .. })` — transport failure, or a
//!   wrapped impl that already classified a retryable status;
//! - `Ok(resp)` with status `429` or `>= 500` — retried as-is, then
//!   handed back on exhaustion so the caller classifies it (the
//!   `Retry-After` hint survives in the headers).
//!
//! Delay is `min(base * 2^attempt, cap)` scaled by jitter (`±50%`),
//! overridden upward by a numeric `Retry-After` (HTTP-date values
//! aren't parsed — computed delay applies instead). Sleeping is
//! behind [`Sleeper`] so tests run instantly.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cicada_core::domain::error::Result;
use cicada_core::ports::{Http, HttpRequest, HttpResponse};

/// Async sleep, injected for deterministic tests.
#[async_trait]
pub trait Sleeper: Send + Sync {
    /// Sleep for `duration` (tests record and return instantly).
    async fn sleep(&self, duration: Duration);
}

/// Real sleep on tokio's timer — the production impl.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Per-call jitter multiplier on the backoff delay.
pub trait Jitter: Send + Sync {
    /// A factor in `[0.5, 1.0]` — the jittered delay.
    fn factor(&self) -> f64;
}

/// Production jitter — `RandomState`-seeded hashing of a monotonically
/// increasing counter; no `rand` dep for one percentage.
#[derive(Debug)]
pub struct RandomJitter {
    counter: AtomicU64,
}

impl Default for RandomJitter {
    fn default() -> Self {
        Self {
            counter: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.subsec_nanos().into()),
            ),
        }
    }
}

impl Jitter for RandomJitter {
    fn factor(&self) -> f64 {
        use std::hash::{BuildHasher, Hasher};
        let state = std::collections::hash_map::RandomState::new();
        let mut h = state.build_hasher();
        h.write_u64(self.counter.fetch_add(1, Ordering::Relaxed));
        let unit = (h.finish() % 500) as f64 / 1000.0; // 0.000–0.499
        0.5 + unit
    }
}

/// Backoff shape for [`RetryHttp`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    /// Total tries per request (first attempt + retries).
    pub max_attempts: u32,
    /// First backoff delay (`base * 2^attempt` from there).
    pub base_delay: Duration,
    /// Per-delay ceiling.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    /// Four attempts, `250ms → ~4s` ceiling — sane against
    /// Calendar's per-minute quotas without stalling a CLI run.
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(4),
        }
    }
}

/// Whether a *response* is retryable — `429` and `5xx`.
fn retryable_status(resp: &HttpResponse) -> bool {
    resp.status == 429 || resp.status >= 500
}

/// Numeric `Retry-After` hint, if the response carries one.
fn retry_after_hint(resp: &HttpResponse) -> Option<Duration> {
    resp.header("retry-after")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// An [`Http`] decorator adding transient retry with jittered
/// exponential backoff. Construct over any inner impl —
/// `ReqwestHttp` in production, `FixtureHttp` in tests.
pub struct RetryHttp<H> {
    /// The wrapped client.
    inner: H,
    /// Attempt/delay policy.
    policy: RetryPolicy,
    /// Injected timer.
    sleeper: Arc<dyn Sleeper>,
    /// Injected jitter.
    jitter: Arc<dyn Jitter>,
}

impl<H: Http> RetryHttp<H> {
    /// Wrap `inner` with the default policy, tokio timer, random jitter.
    pub fn new(inner: H) -> Self {
        Self {
            inner,
            policy: RetryPolicy::default(),
            sleeper: Arc::new(TokioSleeper),
            jitter: Arc::new(RandomJitter::default()),
        }
    }

    /// Override the backoff policy.
    #[must_use]
    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Override the timer (tests: a recording no-op).
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// Override the jitter (tests: a fixed factor).
    #[must_use]
    pub fn with_jitter(mut self, jitter: Arc<dyn Jitter>) -> Self {
        self.jitter = jitter;
        self
    }

    /// The underlying client.
    pub fn inner(&self) -> &H {
        &self.inner
    }

    /// The effective backoff for `attempt` (0-based retry index).
    fn delay(&self, attempt: u32, hint: Option<Duration>) -> Duration {
        let exp = self.policy.base_delay.saturating_mul(1 << attempt.min(10));
        let capped = exp.min(self.policy.max_delay);
        let jittered = capped.mul_f64(self.jitter.factor());
        hint.map_or(jittered, |h| h.max(jittered))
    }
}

impl<H> std::fmt::Debug for RetryHttp<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryHttp")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl<H: Http> Http for RetryHttp<H> {
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse> {
        let mut attempt = 0u32;
        loop {
            let result = self.inner.send(request).await;
            let retry_hint = match &result {
                Err(e) if e.is_retryable() => e.retry_after(),
                Ok(resp) if retryable_status(resp) => retry_after_hint(resp),
                _ => return result,
            };
            if attempt + 1 >= self.policy.max_attempts {
                return result;
            }
            let delay = self.delay(attempt, retry_hint);
            tracing::debug!(
                attempt = attempt + 1,
                delay_ms = delay.as_millis() as u64,
                "retrying transient http failure"
            );
            self.sleeper.sleep(delay).await;
            attempt += 1;
        }
    }
}

/// A [`Sleeper`] that records delays instead of waiting — tests.
#[derive(Debug, Default)]
pub struct RecordingSleeper {
    /// Every `sleep` call's duration, in order.
    pub slept: Mutex<Vec<Duration>>,
}

#[async_trait]
impl Sleeper for RecordingSleeper {
    async fn sleep(&self, duration: Duration) {
        self.slept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(duration);
    }
}

/// Fixed-factor jitter — deterministic tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedJitter(pub f64);

impl Jitter for FixedJitter {
    fn factor(&self) -> f64 {
        self.0
    }
}
