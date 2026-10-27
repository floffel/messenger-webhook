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
//! | `429` | retry after the server's `retry_after_ms` | same | same — a rate limit is answered before the request is processed |
//! | `5xx` | retry | retry | **ambiguous** — the write may have landed |
//! | `4xx` (other) | rejected | rejected | rejected |
//! | connect failure | retry | retry | retry — nothing was written |
//! | timeout / reset | retry | retry | **ambiguous** |
//!
//! A send keyed by a [`TxnId`](crate::matrix_out::TxnId) counts as
//! [`Operation::Idempotent`]: repeating it with the same transaction id returns
//! the original event instead of creating a second one. That is the only reason
//! a `5xx` on a send may be retried at all.

use std::time::Duration;

use serde_json::Value;

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

/// How often, and how long, the client waits between attempts.
///
/// The policy is deterministic on purpose: the backoff is
/// `base_delay * 2^(attempt-1)` clamped to `max_delay`, with no jitter, so a
/// caller can reason about — and test — the exact schedule a delivery will
/// follow. A `429` overrides it with the homeserver's own `retry_after_ms`.
///
/// ```
/// use std::time::Duration;
/// use messenger_webhook::matrix_out::RetryPolicy;
///
/// let policy = RetryPolicy {
///     max_attempts: 4,
///     base_delay: Duration::from_millis(250),
///     max_delay: Duration::from_secs(5),
///     max_rate_limit_wait: Duration::from_secs(30),
/// };
/// assert_eq!(policy.backoff(1), Duration::from_millis(250));
/// assert_eq!(policy.backoff(2), Duration::from_millis(500));
/// assert_eq!(policy.backoff(40), Duration::from_secs(5));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts per operation, the first one included. `1` disables
    /// retrying.
    pub max_attempts: u32,
    /// Delay before the second attempt; doubles for every further attempt.
    pub base_delay: Duration,
    /// Ceiling for the exponential backoff.
    pub max_delay: Duration,
    /// Longest `retry_after_ms` the client will honour. A longer demand ends
    /// the operation with [`Verdict::Exhausted`] so the caller can requeue.
    pub max_rate_limit_wait: Duration,
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
    /// The wait before attempt `attempt + 1`, given that `attempt` attempts have
    /// failed.
    ///
    /// Exponential with a ceiling: `base_delay * 2^(attempt-1)`, clamped to
    /// [`RetryPolicy::max_delay`]. `attempt` is 1-based; `0` is treated as `1`.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::RetryPolicy;
    ///
    /// let policy = RetryPolicy::default();
    /// assert_eq!(policy.backoff(0), policy.base_delay);
    /// assert_eq!(policy.backoff(1), policy.base_delay);
    /// assert_eq!(policy.backoff(3), Duration::from_millis(2000));
    /// ```
    #[must_use]
    pub fn backoff(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(31);
        let factor = 1_u32 << exponent;
        self.base_delay.saturating_mul(factor).min(self.max_delay)
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
    /// ```
    #[must_use]
    pub fn classify(&self, attempt: u32, response: &Response, operation: Operation) -> Verdict {
        match response.status {
            200..=299 => Verdict::Success,
            429 => {
                let wait = retry_after_ms(response).unwrap_or_else(|| self.backoff(attempt));
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
    /// use messenger_webhook::matrix_out::transport::TransportError;
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
        }
    }

    /// The wait before another attempt, or [`Verdict::Exhausted`] when the
    /// attempt budget is spent.
    fn retry_or_exhausted(&self, attempt: u32) -> Verdict {
        if attempt >= self.max_attempts.max(1) {
            Verdict::Exhausted
        } else {
            Verdict::Retry {
                delay: self.backoff(attempt),
            }
        }
    }

    /// Whether the policy can be used at all.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.max_attempts == 0 {
            return Err("max_attempts must be at least 1".to_owned());
        }
        if self.base_delay > self.max_delay {
            return Err(format!(
                "base_delay ({:?}) is longer than max_delay ({:?}), so the backoff could never grow",
                self.base_delay, self.max_delay
            ));
        }
        Ok(())
    }
}

/// The pause a `429` demands, from the body's `retry_after_ms` or the
/// `retry-after` header.
///
/// Matrix sends `{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":2000}`;
/// some reverse proxies in front of a homeserver answer with a bare `Retry-After`
/// header in seconds instead. Both are accepted, the body wins.
pub(crate) fn retry_after_ms(response: &Response) -> Option<Duration> {
    if let Some(ms) = serde_json::from_slice::<Value>(&response.body)
        .ok()
        .and_then(|body| numeric_ms(&body, "retry_after_ms"))
    {
        return Some(ms);
    }
    response
        .header("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
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

    #[test]
    fn backoff_doubles_until_the_ceiling() {
        let policy = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
            max_rate_limit_wait: Duration::from_secs(1),
        };
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
                    delay: Duration::from_millis(500),
                },
                Verdict::Retry {
                    delay: Duration::from_millis(500),
                },
                Verdict::Ambiguous,
            ),
            (
                502,
                Verdict::Retry {
                    delay: Duration::from_millis(500),
                },
                Verdict::Retry {
                    delay: Duration::from_millis(500),
                },
                Verdict::Ambiguous,
            ),
        ] {
            let response = response(status);
            assert_eq!(
                policy.classify(1, &response, Operation::Read),
                read,
                "{status}"
            );
            assert_eq!(
                policy.classify(1, &response, Operation::Idempotent),
                idempotent,
                "{status}"
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
        let policy = RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        };
        assert!(matches!(
            policy.classify(1, &response(503), Operation::Read),
            Verdict::Retry { .. }
        ));
        assert_eq!(
            policy.classify(2, &response(503), Operation::Read),
            Verdict::Exhausted
        );
        // A single-attempt policy never retries.
        let single = RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        };
        assert_eq!(
            single.classify(1, &response(503), Operation::Idempotent),
            Verdict::Exhausted
        );
    }

    #[test]
    fn a_rate_limit_still_respects_the_attempt_budget() {
        let single = RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        };
        let response = Response::json(429, r#"{"retry_after_ms":10}"#);
        assert_eq!(
            single.classify(1, &response, Operation::Idempotent),
            Verdict::Exhausted
        );
        let two = RetryPolicy {
            max_attempts: 2,
            ..RetryPolicy::default()
        };
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
    fn a_429_without_retry_after_falls_back_to_the_backoff() {
        let policy = RetryPolicy::default();
        let bare = Response::json(429, r#"{"errcode":"M_LIMIT_EXCEEDED"}"#);
        assert_eq!(
            policy.classify(2, &bare, Operation::Read),
            Verdict::Retry {
                delay: Duration::from_secs(1)
            }
        );
        let garbage = Response::json(429, "not json at all");
        assert_eq!(
            policy.classify(1, &garbage, Operation::Read),
            Verdict::Retry {
                delay: Duration::from_millis(500)
            }
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
        assert_eq!(
            policy.classify(1, &nonsense, Operation::Read),
            Verdict::Retry {
                delay: Duration::from_millis(500)
            }
        );
    }

    #[test]
    fn a_rate_limit_longer_than_the_budget_is_exhausted() {
        let policy = RetryPolicy {
            max_rate_limit_wait: Duration::from_secs(1),
            ..RetryPolicy::default()
        };
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
    }

    #[test]
    fn an_exhausted_transport_budget_is_exhausted_for_every_operation() {
        let policy = RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        };
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
    fn policy_validation_rejects_the_two_unusable_shapes() {
        assert!(RetryPolicy::default().validate().is_ok());
        let no_attempts = RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::default()
        };
        assert!(no_attempts.validate().is_err());
        let inverted = RetryPolicy {
            base_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(1),
            ..RetryPolicy::default()
        };
        assert!(inverted.validate().is_err());
    }
}
