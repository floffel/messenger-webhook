//! Retry classification and backoff.
//!
//! Whether an attempt may be repeated is not a property of the status code
//! alone. A `503` on `GET /account/whoami` is a retry; a `503` on a media upload
//! is an unknown outcome, because the bytes may already be stored. The pair
//! (status, [`Operation`]) decides, and this module is the single place that
//! decides it — so the client, the tests and the documentation cannot drift
//! apart.
//!
//! | outcome | [`Operation::Read`] | [`Operation::Idempotent`] | [`Operation::NonIdempotent`] |
//! |---------|------------------|--------------------------|------------------------------|
//! | `2xx` | success | success | success |
//! | `429` with a Matrix `M_LIMIT_EXCEEDED` body | retry after the server's `retry_after_ms` | same | same — a homeserver rate limit is answered before the request is processed |
//! | `429` without a Matrix body | retry from the header or backoff | same | **ambiguous** — a reverse proxy may answer after receiving the body |
//! | `5xx` | retry | retry | **ambiguous** — the write may have landed |
//! | `4xx` (other) | rejected | rejected | rejected |
//! | connect failure | retry | retry | retry — nothing was written |
//! | timeout / reset | retry | retry | **ambiguous** |
//! | invalid request before write | rejected | rejected | rejected — fix the request |
//!
//! A send keyed by a [`TxnId`](crate::matrix_out::TxnId) counts as
//! [`Operation::Idempotent`]: repeating it with the same transaction id returns
//! the original event instead of creating a second one. That is the only reason
//! a `5xx` on a send may be retried at all.

use std::time::Duration;

use exponential_backoff::Backoff;
use serde_json::Value;
use thiserror::Error;

use crate::matrix_out::transport::{Response, TransportError, TransportErrorKind};

/// Default number of attempts per operation, including the first one.
///
/// ```
/// use messenger_webhook::matrix_out::DEFAULT_MAX_ATTEMPTS;
///
/// assert_eq!(DEFAULT_MAX_ATTEMPTS, 3);
/// ```
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Default delay before the second attempt, in milliseconds.
///
/// ```
/// use messenger_webhook::matrix_out::DEFAULT_BASE_DELAY_MS;
///
/// assert_eq!(DEFAULT_BASE_DELAY_MS, 500);
/// ```
pub const DEFAULT_BASE_DELAY_MS: u64 = 500;

/// Default ceiling for the exponential backoff, in milliseconds.
///
/// ```
/// use messenger_webhook::matrix_out::DEFAULT_MAX_DELAY_MS;
///
/// assert_eq!(DEFAULT_MAX_DELAY_MS, 30_000);
/// ```
pub const DEFAULT_MAX_DELAY_MS: u64 = 30_000;

/// Default longest wait the client accepts from a `429`, in milliseconds.
///
/// A homeserver that asks for a longer pause is telling the caller to requeue
/// the work, not to block a Lambda invocation for a minute.
///
/// ```
/// use messenger_webhook::matrix_out::DEFAULT_MAX_RATE_LIMIT_WAIT_MS;
///
/// assert_eq!(DEFAULT_MAX_RATE_LIMIT_WAIT_MS, 60_000);
/// ```
pub const DEFAULT_MAX_RATE_LIMIT_WAIT_MS: u64 = 60_000;

/// Largest `429` body searched for `retry_after_ms`.
///
/// Rate-limit bodies are tiny. Bounding the bytes before JSON parsing keeps a
/// hostile or broken endpoint from making the back-pressure path allocate a
/// generic JSON tree for an unbounded response body.
const MAX_RETRY_AFTER_BODY_BYTES: usize = 8 * 1024;

/// How repeatable an operation is.
///
/// ```
/// use messenger_webhook::matrix_out::Operation;
///
/// assert!(Operation::Read.is_repeatable());
/// assert!(!Operation::NonIdempotent.is_repeatable());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Operation {
    /// Changes nothing: `GET /account/whoami`. Always safe to repeat.
    Read,
    /// Changes state, but repeating it converges: a transaction-keyed send, a
    /// typing notification, a read receipt.
    Idempotent,
    /// Creates something new on every attempt: a media upload. Repeating it
    /// leaves an orphaned upload and gives the caller no way to know whether the
    /// first one landed.
    NonIdempotent,
}

impl Operation {
    /// Whether an uncertain outcome may be retried at all.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::Operation;
    ///
    /// assert!(Operation::Idempotent.is_repeatable());
    /// assert!(!Operation::NonIdempotent.is_repeatable());
    /// ```
    #[must_use]
    pub const fn is_repeatable(self) -> bool {
        !matches!(self, Self::NonIdempotent)
    }

    /// Stable lower-case name, used in log lines.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::Operation;
    ///
    /// assert_eq!(Operation::NonIdempotent.as_str(), "non-idempotent");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Idempotent => "idempotent",
            Self::NonIdempotent => "non-idempotent",
        }
    }
}

/// What to do after one attempt.
///
/// ```
/// use std::time::Duration;
/// use messenger_webhook::matrix_out::Verdict;
///
/// let verdict = Verdict::Retry { delay: Duration::from_millis(500) };
/// assert!(!verdict.is_final());
/// assert!(Verdict::Success.is_final());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// The homeserver accepted the request.
    Success,
    /// The request was not applied. Repeat it after `delay`.
    Retry {
        /// How long to wait before the next attempt.
        delay: Duration,
    },
    /// The request was not applied and will not be: fix the request.
    Rejected,
    /// The request was not applied, but the policy allows no further attempt —
    /// either the attempt budget or the rate-limit wait budget ran out.
    Exhausted,
    /// The request may or may not have been applied. Never repeat it blindly.
    Ambiguous,
}

impl Verdict {
    /// Whether this verdict ends the operation.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::Verdict;
    ///
    /// assert!(Verdict::Ambiguous.is_final());
    /// assert!(Verdict::Exhausted.is_final());
    /// ```
    #[must_use]
    pub const fn is_final(self) -> bool {
        !matches!(self, Self::Retry { .. })
    }

    /// The delay of a [`Verdict::Retry`], `None` for every other verdict.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::Verdict;
    ///
    /// assert_eq!(Verdict::Retry { delay: Duration::ZERO }.delay(), Some(Duration::ZERO));
    /// assert_eq!(Verdict::Success.delay(), None);
    /// ```
    #[must_use]
    pub const fn delay(self) -> Option<Duration> {
        match self {
            Self::Retry { delay } => Some(delay),
            _ => None,
        }
    }
}

/// A retry policy could not be constructed because one of its invariants
/// would be broken.
///
/// ```
/// use std::time::Duration;
/// use messenger_webhook::matrix_out::{RetryPolicy, RetryPolicyError};
///
/// let error = RetryPolicy::new(
///     0,
///     Duration::from_millis(100),
///     Duration::from_secs(1),
///     Duration::from_secs(1),
/// )
/// .expect_err("zero attempts is unusable");
/// assert_eq!(error, RetryPolicyError::ZeroMaxAttempts);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RetryPolicyError {
    /// `max_attempts` was zero, so no operation could ever be attempted.
    #[error("retry policy max_attempts must be at least 1")]
    ZeroMaxAttempts,
    /// `base_delay` was zero, so retries would spin without waiting.
    #[error("retry policy base_delay must be greater than zero")]
    ZeroBaseDelay,
    /// `base_delay` was longer than `max_delay`, so the backoff could never
    /// grow from its starting point.
    #[error("retry policy base_delay ({base_delay:?}) is longer than max_delay ({max_delay:?})")]
    BaseDelayExceedsMaxDelay {
        /// The configured first delay.
        base_delay: Duration,
        /// The configured delay ceiling.
        max_delay: Duration,
    },
    /// `max_rate_limit_wait` was zero, so every `429` would be exhausted.
    #[error("retry policy max_rate_limit_wait must be greater than zero")]
    ZeroMaxRateLimitWait,
}

/// How often, and how long, the client waits between attempts.
///
/// The schedule is `base_delay * 2^(attempt-1)`, clamped to `max_delay`, from
/// [`exponential_backoff`]. [`RetryPolicy::backoff`] exposes the deterministic
/// schedule for callers that need to reason about or test it; classification
/// adds the crate's default jitter when the homeserver did not provide its own
/// delay. A `429` overrides both with the homeserver's `retry_after_ms`.
///
/// Build one with [`RetryPolicy::new`] or [`RetryPolicy::default`]; the
/// constructor rejects values that would make one of the policy's own
/// invariants unenforceable.
///
/// ```
/// use std::time::Duration;
/// use messenger_webhook::matrix_out::RetryPolicy;
///
/// let policy = RetryPolicy::new(
///     4,
///     Duration::from_millis(250),
///     Duration::from_secs(5),
///     Duration::from_secs(30),
/// )?;
/// assert_eq!(policy.backoff(1), Duration::from_millis(250));
/// assert_eq!(policy.backoff(2), Duration::from_millis(500));
/// assert_eq!(policy.backoff(40), Duration::from_secs(5));
/// # Ok::<(), messenger_webhook::matrix_out::RetryPolicyError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
    max_rate_limit_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: Duration::from_millis(DEFAULT_BASE_DELAY_MS),
            max_delay: Duration::from_millis(DEFAULT_MAX_DELAY_MS),
            max_rate_limit_wait: Duration::from_millis(DEFAULT_MAX_RATE_LIMIT_WAIT_MS),
        }
    }
}

impl RetryPolicy {
    /// Builds a retry policy after checking every invariant the policy relies
    /// on.
    ///
    /// # Errors
    ///
    /// [`RetryPolicyError::ZeroMaxAttempts`] when `max_attempts` is `0`,
    /// [`RetryPolicyError::ZeroBaseDelay`] when `base_delay` is zero,
    /// [`RetryPolicyError::BaseDelayExceedsMaxDelay`] when `base_delay` is
    /// longer than `max_delay`, and
    /// [`RetryPolicyError::ZeroMaxRateLimitWait`] when `max_rate_limit_wait` is
    /// zero.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// let policy = RetryPolicy::new(
    ///     2,
    ///     Duration::from_millis(100),
    ///     Duration::from_millis(100),
    ///     Duration::from_millis(1),
    /// )?;
    /// assert_eq!(policy.max_attempts(), 2);
    /// assert_eq!(policy.max_delay(), Duration::from_millis(100));
    /// # Ok::<(), messenger_webhook::matrix_out::RetryPolicyError>(())
    /// ```
    pub fn new(
        max_attempts: u32,
        base_delay: Duration,
        max_delay: Duration,
        max_rate_limit_wait: Duration,
    ) -> Result<Self, RetryPolicyError> {
        if max_attempts == 0 {
            return Err(RetryPolicyError::ZeroMaxAttempts);
        }
        if base_delay.is_zero() {
            return Err(RetryPolicyError::ZeroBaseDelay);
        }
        if base_delay > max_delay {
            return Err(RetryPolicyError::BaseDelayExceedsMaxDelay {
                base_delay,
                max_delay,
            });
        }
        if max_rate_limit_wait.is_zero() {
            return Err(RetryPolicyError::ZeroMaxRateLimitWait);
        }
        Ok(Self {
            max_attempts,
            base_delay,
            max_delay,
            max_rate_limit_wait,
        })
    }

    /// Replaces [`RetryPolicy::max_attempts()`], preserving the other fields.
    ///
    /// # Errors
    ///
    /// [`RetryPolicyError::ZeroMaxAttempts`] when `max_attempts` is `0`.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// let policy = RetryPolicy::default().with_max_attempts(2)?;
    /// assert_eq!(policy.max_attempts(), 2);
    /// # Ok::<(), messenger_webhook::matrix_out::RetryPolicyError>(())
    /// ```
    pub fn with_max_attempts(self, max_attempts: u32) -> Result<Self, RetryPolicyError> {
        Self::new(
            max_attempts,
            self.base_delay,
            self.max_delay,
            self.max_rate_limit_wait,
        )
    }

    /// Replaces [`RetryPolicy::base_delay()`], preserving the other fields.
    ///
    /// # Errors
    ///
    /// [`RetryPolicyError::ZeroBaseDelay`] when `base_delay` is zero, and
    /// [`RetryPolicyError::BaseDelayExceedsMaxDelay`] when `base_delay` is
    /// longer than the current `max_delay`.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// let policy = RetryPolicy::default().with_base_delay(Duration::from_millis(1))?;
    /// assert_eq!(policy.base_delay(), Duration::from_millis(1));
    /// # Ok::<(), messenger_webhook::matrix_out::RetryPolicyError>(())
    /// ```
    pub fn with_base_delay(self, base_delay: Duration) -> Result<Self, RetryPolicyError> {
        Self::new(
            self.max_attempts,
            base_delay,
            self.max_delay,
            self.max_rate_limit_wait,
        )
    }

    /// Replaces [`RetryPolicy::max_delay()`], preserving the other fields.
    ///
    /// # Errors
    ///
    /// [`RetryPolicyError::BaseDelayExceedsMaxDelay`] when the current
    /// `base_delay` is longer than `max_delay`.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// let policy = RetryPolicy::default().with_max_delay(Duration::from_secs(10))?;
    /// assert_eq!(policy.max_delay(), Duration::from_secs(10));
    /// # Ok::<(), messenger_webhook::matrix_out::RetryPolicyError>(())
    /// ```
    pub fn with_max_delay(self, max_delay: Duration) -> Result<Self, RetryPolicyError> {
        Self::new(
            self.max_attempts,
            self.base_delay,
            max_delay,
            self.max_rate_limit_wait,
        )
    }

    /// Replaces [`RetryPolicy::max_rate_limit_wait()`], preserving the other
    /// fields.
    ///
    /// # Errors
    ///
    /// [`RetryPolicyError::ZeroMaxRateLimitWait`] when `max_rate_limit_wait` is
    /// zero.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// let policy = RetryPolicy::default().with_max_rate_limit_wait(Duration::from_secs(1))?;
    /// assert_eq!(policy.max_rate_limit_wait(), Duration::from_secs(1));
    /// # Ok::<(), messenger_webhook::matrix_out::RetryPolicyError>(())
    /// ```
    pub fn with_max_rate_limit_wait(
        self,
        max_rate_limit_wait: Duration,
    ) -> Result<Self, RetryPolicyError> {
        Self::new(
            self.max_attempts,
            self.base_delay,
            self.max_delay,
            max_rate_limit_wait,
        )
    }

    /// Total attempts per operation, the first one included. `1` disables
    /// retrying.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// assert_eq!(RetryPolicy::default().max_attempts(), 3);
    /// ```
    #[must_use]
    pub const fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// Delay before the second attempt; doubles for every further attempt.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// assert_eq!(RetryPolicy::default().base_delay(), Duration::from_millis(500));
    /// ```
    #[must_use]
    pub const fn base_delay(&self) -> Duration {
        self.base_delay
    }

    /// Ceiling for the exponential backoff.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// assert_eq!(RetryPolicy::default().max_delay(), Duration::from_secs(30));
    /// ```
    #[must_use]
    pub const fn max_delay(&self) -> Duration {
        self.max_delay
    }

    /// Longest `retry_after_ms` the client will honour. A longer demand ends
    /// the operation with [`Verdict::Exhausted`] so the caller can requeue.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// assert_eq!(RetryPolicy::default().max_rate_limit_wait(), Duration::from_secs(60));
    /// ```
    #[must_use]
    pub const fn max_rate_limit_wait(&self) -> Duration {
        self.max_rate_limit_wait
    }

    /// The wait before attempt `attempt + 1`, given that `attempt` attempts have
    /// failed.
    ///
    /// Exponential with a ceiling: `base_delay * 2^(attempt-1)`, clamped to
    /// [`RetryPolicy::max_delay`]. `attempt` is 1-based; `0` is treated as `1`.
    /// This method is deterministic; retry classification uses the same
    /// schedule with jitter.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// let policy = RetryPolicy::default();
    /// assert_eq!(policy.backoff(0), policy.base_delay());
    /// assert_eq!(policy.backoff(1), policy.base_delay());
    /// assert_eq!(policy.backoff(3), Duration::from_millis(2000));
    /// ```
    #[must_use]
    pub fn backoff(&self, attempt: u32) -> Duration {
        self.backoff_with_jitter(attempt, 0.0)
    }

    /// The delay to wait after a failed attempt, with the maintained crate's
    /// jitter applied to the exponential schedule.
    fn retry_delay(&self, attempt: u32) -> Duration {
        self.backoff_with_jitter(attempt, 0.3)
    }

    /// Builds one delay from [`exponential_backoff`].
    ///
    /// `jitter` is the maintained crate's fraction. The iterator yields one
    /// delay per attempt, so `attempt` maps to the same 1-based step as the
    /// public API: `0` and `1` are the base delay, `2` doubles it, and so on.
    fn backoff_with_jitter(&self, attempt: u32, jitter: f32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(31);
        let mut backoff = Backoff::new(exponent.saturating_add(2), self.base_delay, self.max_delay);
        backoff.set_jitter(jitter);
        backoff
            .into_iter()
            .nth(exponent as usize)
            .flatten()
            .unwrap_or(self.max_delay)
    }

    /// Classifies an HTTP response.
    ///
    /// `attempt` is the attempt that produced `response`, 1-based.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::Response;
    /// use messenger_webhook::matrix_out::{Operation, RetryPolicy, Verdict};
    ///
    /// let policy = RetryPolicy::default();
    /// assert_eq!(
    ///     policy.classify(1, &Response::json(200, "{}"), Operation::Read),
    ///     Verdict::Success,
    /// );
    /// assert_eq!(
    ///     policy.classify(1, &Response::json(503, "{}"), Operation::NonIdempotent),
    ///     Verdict::Ambiguous,
    /// );
    /// let proxy = Response::new(429, Vec::new()).with_header("retry-after", "2");
    /// assert_eq!(
    ///     policy.classify(1, &proxy, Operation::NonIdempotent),
    ///     Verdict::Ambiguous,
    /// );
    /// ```
    #[must_use]
    pub fn classify(&self, attempt: u32, response: &Response, operation: Operation) -> Verdict {
        match response.status {
            200..=299 => Verdict::Success,
            429 if !operation.is_repeatable() && !matrix_rate_limit(response) => Verdict::Ambiguous,
            429 => {
                let wait = retry_after_ms(response).unwrap_or_else(|| self.retry_delay(attempt));
                // A rate limit still costs an attempt: the budget bounds every
                // request, not only the exponential ones.
                if attempt >= self.max_attempts.max(1) || wait > self.max_rate_limit_wait {
                    Verdict::Exhausted
                } else {
                    Verdict::Retry { delay: wait }
                }
            }
            500..=599 if !operation.is_repeatable() => Verdict::Ambiguous,
            500..=599 => self.retry_or_exhausted(attempt),
            _ => Verdict::Rejected,
        }
    }

    /// Classifies a transport failure — one that produced no HTTP response.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::transport::{TransportError, TransportErrorKind};
    /// use messenger_webhook::matrix_out::{Operation, RetryPolicy, Verdict};
    ///
    /// let policy = RetryPolicy::default();
    /// // Nothing was written, so even an upload may be repeated.
    /// assert!(matches!(
    ///     policy.classify_transport(1, &TransportError::connect("dns"), Operation::NonIdempotent),
    ///     Verdict::Retry { .. },
    /// ));
    /// // A timeout may have been applied.
    /// assert_eq!(
    ///     policy.classify_transport(1, &TransportError::timeout("30s"), Operation::NonIdempotent),
    ///     Verdict::Ambiguous,
    /// );
    /// assert_eq!(
    ///     policy.classify_transport(
    ///         1,
    ///         &TransportError::new(TransportErrorKind::InvalidRequest, "bad header"),
    ///         Operation::NonIdempotent,
    ///     ),
    ///     Verdict::Rejected,
    /// );
    /// ```
    #[must_use]
    pub fn classify_transport(
        &self,
        attempt: u32,
        error: &TransportError,
        operation: Operation,
    ) -> Verdict {
        match error.kind {
            TransportErrorKind::Connect => self.retry_or_exhausted(attempt),
            TransportErrorKind::Timeout | TransportErrorKind::Other
                if operation.is_repeatable() =>
            {
                self.retry_or_exhausted(attempt)
            }
            TransportErrorKind::Timeout | TransportErrorKind::Other => Verdict::Ambiguous,
            TransportErrorKind::InvalidRequest | TransportErrorKind::TestHarness => {
                Verdict::Rejected
            }
        }
    }

    /// The wait before another attempt, or [`Verdict::Exhausted`] when the
    /// attempt budget is spent.
    fn retry_or_exhausted(&self, attempt: u32) -> Verdict {
        if attempt >= self.max_attempts.max(1) {
            Verdict::Exhausted
        } else {
            Verdict::Retry {
                delay: self.retry_delay(attempt),
            }
        }
    }
}

/// The pause a `429` demands, from the body's `retry_after_ms` or the
/// `retry-after` header.
///
/// Matrix sends `{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":2000}`;
/// some reverse proxies in front of a homeserver answer with a bare `Retry-After`
/// header in seconds instead. Both are accepted, the body wins.
pub(crate) fn retry_after_ms(response: &Response) -> Option<Duration> {
    if let Some(ms) = retry_after_ms_from_body(&response.body) {
        return Some(ms);
    }
    response
        .header("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Whether the response body proves the homeserver answered the `429` with
/// `M_LIMIT_EXCEEDED`.
///
/// Reverse proxies can answer `429` after forwarding the request body, so a
/// non-idempotent operation needs the Matrix error code before it may retry.
pub(crate) fn matrix_rate_limit(response: &Response) -> bool {
    matrix_rate_limit_from_body(&response.body)
}

fn matrix_rate_limit_from_body(body: &[u8]) -> bool {
    if body.len() > MAX_RETRY_AFTER_BODY_BYTES {
        return false;
    }
    serde_json::from_slice::<Value>(body).is_ok_and(|body| {
        body.get("errcode")
            .and_then(Value::as_str)
            .is_some_and(|errcode| errcode == "M_LIMIT_EXCEEDED")
    })
}

/// The body's `retry_after_ms`, only when the body is small enough to parse.
fn retry_after_ms_from_body(body: &[u8]) -> Option<Duration> {
    if body.len() > MAX_RETRY_AFTER_BODY_BYTES {
        return None;
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|body| numeric_ms(&body, "retry_after_ms"))
}

/// `field` as a millisecond value, accepting a number or a numeric string.
fn numeric_ms(body: &Value, field: &str) -> Option<Duration> {
    let value = body.get(field)?;
    if let Some(number) = value.as_u64() {
        return Some(Duration::from_millis(number));
    }
    if let Some(number) = value.as_f64() {
        // Fractional milliseconds are legal JSON; `try_from_secs_f64` converts
        // without a cast and rejects NaN, infinity and negatives for free.
        return Duration::try_from_secs_f64(number / 1_000.0).ok();
    }
    value
        .as_str()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16) -> Response {
        Response::json(status, "{}")
    }

    fn assert_retry_window(policy: &RetryPolicy, actual: Verdict, expected: Verdict) {
        match (actual, expected) {
            (Verdict::Retry { delay }, Verdict::Retry { delay: base }) => {
                let lower = base.mul_f64(0.7);
                let upper = base.mul_f64(1.3).min(policy.max_delay());
                assert!(
                    delay >= lower && delay <= upper,
                    "{delay:?} outside {lower:?}..={upper:?}"
                );
            }
            (actual, expected) => assert_eq!(actual, expected),
        }
    }

    #[test]
    fn backoff_doubles_until_the_ceiling() {
        let policy = RetryPolicy::new(
            10,
            Duration::from_millis(100),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("valid");
        assert_eq!(policy.backoff(0), Duration::from_millis(100));
        assert_eq!(policy.backoff(1), Duration::from_millis(100));
        assert_eq!(policy.backoff(2), Duration::from_millis(200));
        assert_eq!(policy.backoff(4), Duration::from_millis(800));
        assert_eq!(policy.backoff(5), Duration::from_secs(1));
        assert_eq!(policy.backoff(u32::MAX), Duration::from_secs(1));
    }

    #[test]
    fn default_backoff_sequence_is_the_documented_one() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.backoff(1), Duration::from_millis(500));
        assert_eq!(policy.backoff(2), Duration::from_secs(1));
        assert_eq!(policy.backoff(3), Duration::from_secs(2));
        assert_eq!(policy.backoff(20), Duration::from_secs(30));
    }

    #[test]
    fn runtime_backoff_is_the_same_window_with_jitter() {
        let policy = RetryPolicy::new(
            10,
            Duration::from_millis(100),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("valid");
        for attempt in 1..=5 {
            let deterministic = policy.backoff(attempt);
            let jittered = policy.retry_delay(attempt);
            let lower = deterministic.mul_f64(0.7);
            let upper = deterministic.mul_f64(1.3).min(policy.max_delay());
            assert!(
                jittered >= lower && jittered <= upper,
                "attempt {attempt}: {jittered:?} outside {lower:?}..={upper:?}"
            );
        }
    }

    #[test]
    fn every_status_class_is_honoured_per_operation() {
        let policy = RetryPolicy::default();
        for (status, read, idempotent, non_idempotent) in [
            (204, Verdict::Success, Verdict::Success, Verdict::Success),
            (400, Verdict::Rejected, Verdict::Rejected, Verdict::Rejected),
            (403, Verdict::Rejected, Verdict::Rejected, Verdict::Rejected),
            (404, Verdict::Rejected, Verdict::Rejected, Verdict::Rejected),
            (413, Verdict::Rejected, Verdict::Rejected, Verdict::Rejected),
            (
                500,
                Verdict::Retry {
                    delay: policy.backoff(1),
                },
                Verdict::Retry {
                    delay: policy.backoff(1),
                },
                Verdict::Ambiguous,
            ),
            (
                502,
                Verdict::Retry {
                    delay: policy.backoff(1),
                },
                Verdict::Retry {
                    delay: policy.backoff(1),
                },
                Verdict::Ambiguous,
            ),
        ] {
            let response = response(status);
            assert_retry_window(
                &policy,
                policy.classify(1, &response, Operation::Read),
                read,
            );
            assert_retry_window(
                &policy,
                policy.classify(1, &response, Operation::Idempotent),
                idempotent,
            );
            assert_eq!(
                policy.classify(1, &response, Operation::NonIdempotent),
                non_idempotent,
                "{status}"
            );
        }
    }

    #[test]
    fn the_attempt_budget_ends_in_exhausted_not_ambiguous() {
        let policy = RetryPolicy::default().with_max_attempts(2).expect("valid");
        assert!(matches!(
            policy.classify(1, &response(503), Operation::Read),
            Verdict::Retry { .. }
        ));
        assert_eq!(
            policy.classify(2, &response(503), Operation::Read),
            Verdict::Exhausted
        );
        // A single-attempt policy never retries.
        let single = RetryPolicy::default().with_max_attempts(1).expect("valid");
        assert_eq!(
            single.classify(1, &response(503), Operation::Idempotent),
            Verdict::Exhausted
        );
    }

    #[test]
    fn a_rate_limit_still_respects_the_attempt_budget() {
        let single = RetryPolicy::default().with_max_attempts(1).expect("valid");
        let response = Response::json(429, r#"{"retry_after_ms":10}"#);
        assert_eq!(
            single.classify(1, &response, Operation::Idempotent),
            Verdict::Exhausted
        );
        let two = RetryPolicy::default().with_max_attempts(2).expect("valid");
        assert!(matches!(
            two.classify(1, &response, Operation::Idempotent),
            Verdict::Retry { .. }
        ));
        assert_eq!(
            two.classify(2, &response, Operation::Idempotent),
            Verdict::Exhausted
        );
    }

    #[test]
    fn a_rate_limit_uses_the_servers_own_delay() {
        let policy = RetryPolicy::default();
        let response = Response::json(
            429,
            r#"{"errcode":"M_LIMIT_EXCEEDED","error":"Too many requests","retry_after_ms":2000}"#,
        );
        assert_eq!(
            policy.classify(1, &response, Operation::NonIdempotent),
            Verdict::Retry {
                delay: Duration::from_secs(2)
            }
        );
        // A 429 is answered before the request is processed, so even an upload
        // may be repeated after it.
    }

    #[test]
    fn a_proxy_429_is_ambiguous_for_non_idempotent_work() {
        let policy = RetryPolicy::default();
        let proxy = Response::new(429, Vec::new()).with_header("retry-after", "2");
        assert_eq!(
            policy.classify(1, &proxy, Operation::NonIdempotent),
            Verdict::Ambiguous
        );
        assert!(matches!(
            policy.classify(1, &proxy, Operation::Read),
            Verdict::Retry { .. }
        ));

        let non_matrix = Response::json(429, r#"{"retry_after_ms":2000}"#);
        assert_eq!(
            policy.classify(1, &non_matrix, Operation::NonIdempotent),
            Verdict::Ambiguous
        );
        assert!(matches!(
            policy.classify(1, &non_matrix, Operation::Read),
            Verdict::Retry { .. }
        ));
    }

    #[test]
    fn a_matrix_rate_limit_is_still_retryable_for_non_idempotent_work() {
        let policy = RetryPolicy::default();
        let response = Response::json(
            429,
            r#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":2000}"#,
        );
        assert_eq!(
            policy.classify(1, &response, Operation::NonIdempotent),
            Verdict::Retry {
                delay: Duration::from_secs(2)
            }
        );

        let boundary = Response::new(
            429,
            br#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":1}"#.to_vec(),
        );
        assert_eq!(
            policy.classify(1, &boundary, Operation::NonIdempotent),
            Verdict::Retry {
                delay: Duration::from_millis(1)
            }
        );
    }

    #[test]
    fn a_matrix_rate_limit_body_is_size_bounded_before_parsing() {
        let policy = RetryPolicy::default();
        let prefix = br#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":250}"#;
        let mut at_limit = Vec::with_capacity(MAX_RETRY_AFTER_BODY_BYTES);
        at_limit.extend_from_slice(prefix);
        at_limit.resize(MAX_RETRY_AFTER_BODY_BYTES, b' ');
        assert_eq!(
            policy.classify(1, &Response::new(429, at_limit), Operation::NonIdempotent),
            Verdict::Retry {
                delay: Duration::from_millis(250)
            }
        );

        let mut over_limit = Vec::with_capacity(MAX_RETRY_AFTER_BODY_BYTES + 1);
        over_limit.extend_from_slice(prefix);
        over_limit.resize(MAX_RETRY_AFTER_BODY_BYTES + 1, b' ');
        assert_eq!(
            policy.classify(1, &Response::new(429, over_limit), Operation::NonIdempotent),
            Verdict::Ambiguous
        );
    }

    #[test]
    fn a_429_without_retry_after_falls_back_to_the_backoff() {
        let policy = RetryPolicy::default();
        let bare = Response::json(429, r#"{"errcode":"M_LIMIT_EXCEEDED"}"#);
        assert_retry_window(
            &policy,
            policy.classify(2, &bare, Operation::Read),
            Verdict::Retry {
                delay: policy.backoff(2),
            },
        );
        let garbage = Response::json(429, "not json at all");
        assert_retry_window(
            &policy,
            policy.classify(1, &garbage, Operation::Read),
            Verdict::Retry {
                delay: policy.backoff(1),
            },
        );
        let empty = Response::new(429, Vec::new());
        assert!(matches!(
            policy.classify(1, &empty, Operation::Read),
            Verdict::Retry { .. }
        ));
    }

    #[test]
    fn a_rate_limit_header_is_honoured_in_seconds() {
        let policy = RetryPolicy::default();
        let response = Response::new(429, Vec::new()).with_header("Retry-After", "3");
        assert_eq!(
            policy.classify(1, &response, Operation::Read),
            Verdict::Retry {
                delay: Duration::from_secs(3)
            }
        );
        let nonsense = Response::new(429, Vec::new()).with_header("retry-after", "soon");
        assert_retry_window(
            &policy,
            policy.classify(1, &nonsense, Operation::Read),
            Verdict::Retry {
                delay: policy.backoff(1),
            },
        );
    }

    #[test]
    fn a_rate_limit_longer_than_the_budget_is_exhausted() {
        let policy = RetryPolicy::default()
            .with_max_rate_limit_wait(Duration::from_secs(1))
            .expect("valid");
        let response = Response::json(429, r#"{"retry_after_ms":1001}"#);
        assert_eq!(
            policy.classify(1, &response, Operation::Read),
            Verdict::Exhausted
        );
        let exactly = Response::json(429, r#"{"retry_after_ms":1000}"#);
        assert!(matches!(
            policy.classify(1, &exactly, Operation::Read),
            Verdict::Retry { .. }
        ));
    }

    #[test]
    fn retry_after_accepts_numbers_and_numeric_strings() {
        for body in [
            r#"{"retry_after_ms":250}"#,
            r#"{"retry_after_ms":250.4}"#,
            r#"{"retry_after_ms":"250"}"#,
        ] {
            let response = Response::json(429, body);
            assert!(
                retry_after_ms(&response).is_some(),
                "{body} should carry a delay"
            );
        }
        for body in [
            r#"{"retry_after_ms":-1}"#,
            r#"{"retry_after_ms":null}"#,
            r#"{"retry_after_ms":"later"}"#,
            r#"{"other":1}"#,
            "[1,2,3]",
        ] {
            let response = Response::json(429, body);
            assert!(
                retry_after_ms(&response).is_none(),
                "{body} should not carry a delay"
            );
        }
        // A non-finite float is not a duration.
        let response = Response::json(429, r#"{"retry_after_ms":1e400}"#);
        assert!(retry_after_ms(&response).is_none());
    }

    #[test]
    fn retry_after_body_is_size_bounded_before_parsing() {
        let prefix = br#"{"retry_after_ms":250}"#;
        let mut at_limit = Vec::with_capacity(MAX_RETRY_AFTER_BODY_BYTES);
        at_limit.extend_from_slice(prefix);
        at_limit.resize(MAX_RETRY_AFTER_BODY_BYTES, b' ');
        let response = Response::new(429, at_limit);
        assert_eq!(retry_after_ms(&response), Some(Duration::from_millis(250)));

        let mut over_limit = Vec::with_capacity(MAX_RETRY_AFTER_BODY_BYTES + 1);
        over_limit.extend_from_slice(prefix);
        over_limit.resize(MAX_RETRY_AFTER_BODY_BYTES + 1, b' ');
        let response = Response::new(429, over_limit);
        assert!(retry_after_ms(&response).is_none());
    }

    #[test]
    fn transport_failures_are_classified_by_the_write_boundary() {
        let policy = RetryPolicy::default();
        assert!(matches!(
            policy.classify_transport(1, &TransportError::connect("dns"), Operation::NonIdempotent),
            Verdict::Retry { .. }
        ));
        for error in [
            TransportError::timeout("30s"),
            TransportError::other("connection reset"),
        ] {
            assert_eq!(
                policy.classify_transport(1, &error, Operation::NonIdempotent),
                Verdict::Ambiguous
            );
            assert!(matches!(
                policy.classify_transport(1, &error, Operation::Read),
                Verdict::Retry { .. }
            ));
        }
        assert_eq!(
            policy.classify_transport(
                1,
                &TransportError::new(TransportErrorKind::InvalidRequest, "bad header"),
                Operation::NonIdempotent,
            ),
            Verdict::Rejected
        );
    }

    #[test]
    fn an_exhausted_transport_budget_is_exhausted_for_every_operation() {
        let policy = RetryPolicy::default().with_max_attempts(1).expect("valid");
        assert_eq!(
            policy.classify_transport(1, &TransportError::connect("dns"), Operation::Read),
            Verdict::Exhausted
        );
    }

    #[test]
    fn verdict_helpers_agree_with_the_variants() {
        assert!(Verdict::Success.is_final());
        assert!(Verdict::Rejected.is_final());
        assert!(Verdict::Exhausted.is_final());
        assert!(Verdict::Ambiguous.is_final());
        assert!(!Verdict::Retry {
            delay: Duration::ZERO
        }
        .is_final());
        assert_eq!(
            Verdict::Retry {
                delay: Duration::ZERO
            }
            .delay(),
            Some(Duration::ZERO)
        );
        assert_eq!(Verdict::Ambiguous.delay(), None);
    }

    #[test]
    fn operation_names_and_repeatability() {
        assert_eq!(Operation::Read.as_str(), "read");
        assert_eq!(Operation::Idempotent.as_str(), "idempotent");
        assert_eq!(Operation::NonIdempotent.as_str(), "non-idempotent");
        assert!(Operation::Read.is_repeatable());
        assert!(Operation::Idempotent.is_repeatable());
        assert!(!Operation::NonIdempotent.is_repeatable());
    }

    #[test]
    fn policy_constructor_and_updates_enforce_boundaries() {
        assert!(RetryPolicy::new(
            1,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .is_ok());
        assert_eq!(
            RetryPolicy::new(
                0,
                Duration::from_millis(1),
                Duration::from_millis(1),
                Duration::from_millis(1),
            ),
            Err(RetryPolicyError::ZeroMaxAttempts)
        );
        assert_eq!(
            RetryPolicy::new(
                1,
                Duration::ZERO,
                Duration::from_millis(1),
                Duration::from_millis(1),
            ),
            Err(RetryPolicyError::ZeroBaseDelay)
        );
        assert_eq!(
            RetryPolicy::new(
                1,
                Duration::from_millis(2),
                Duration::from_millis(1),
                Duration::from_millis(1),
            ),
            Err(RetryPolicyError::BaseDelayExceedsMaxDelay {
                base_delay: Duration::from_millis(2),
                max_delay: Duration::from_millis(1),
            })
        );
        assert_eq!(
            RetryPolicy::new(
                1,
                Duration::from_millis(1),
                Duration::from_millis(1),
                Duration::ZERO,
            ),
            Err(RetryPolicyError::ZeroMaxRateLimitWait)
        );

        let base = RetryPolicy::default();
        assert_eq!(
            base.with_max_attempts(0),
            Err(RetryPolicyError::ZeroMaxAttempts)
        );
        assert_eq!(
            base.with_base_delay(Duration::ZERO),
            Err(RetryPolicyError::ZeroBaseDelay)
        );
        assert_eq!(
            base.with_max_delay(Duration::from_millis(1)),
            Err(RetryPolicyError::BaseDelayExceedsMaxDelay {
                base_delay: Duration::from_millis(DEFAULT_BASE_DELAY_MS),
                max_delay: Duration::from_millis(1),
            })
        );
        assert_eq!(
            base.with_max_rate_limit_wait(Duration::ZERO),
            Err(RetryPolicyError::ZeroMaxRateLimitWait)
        );
    }
}
