//! The outbound error surface: fail closed, and say whether a retry is safe.
//!
//! Three questions decide what a caller may do next, and every variant answers
//! exactly one way:
//!
//! * **retryable** ([`OutboundError::is_retryable`]) — nothing was applied and
//!   the same request may succeed later. Requeue it.
//! * **ambiguous** ([`OutboundError::is_ambiguous`]) — the homeserver may have
//!   applied the request. Do **not** mint a new transaction id and send again:
//!   repeat the *same* one ([`MatrixClient::send_text_with_txn`]) or reconcile
//!   by reading the room.
//! * everything else — the request was rejected and will be rejected again.
//!
//! [`MatrixClient::send_text_with_txn`]: crate::matrix_out::MatrixClient::send_text_with_txn

use std::time::Duration;

use thiserror::Error;

use crate::matrix_out::types::TxnId;

/// Everything the outbound client can fail with.
///
/// ```
/// use messenger_webhook::matrix_out::OutboundError;
///
/// let error = OutboundError::InvalidRequest { reason: "the room id is empty".into() };
/// assert!(!error.is_retryable());
/// assert!(!error.is_ambiguous());
/// assert!(error.to_string().contains("the room id is empty"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum OutboundError {
    /// The client configuration cannot work — an unusable homeserver URL, an
    /// empty access token, a retry policy that would never retry.
    #[error("invalid Matrix client configuration: {reason}")]
    InvalidConfig {
        /// What is wrong with the configuration.
        reason: String,
    },

    /// A transaction id is empty, too long or carries a byte a URL path segment
    /// cannot hold.
    #[error("invalid transaction id: {reason}")]
    InvalidTxnId {
        /// Which rule the value broke.
        reason: String,
    },

    /// An `mxc://` URI is malformed.
    #[error("invalid Matrix content URI: {reason}")]
    InvalidMxcUri {
        /// Which rule the value broke.
        reason: String,
    },

    /// The request itself cannot be addressed: an empty room id, an id of the
    /// wrong shape, an empty message body, an unusable media type.
    #[error("cannot send this request: {reason}")]
    InvalidRequest {
        /// What has to change before the request is sendable.
        reason: String,
    },

    /// The serialised content is larger than the event ceiling.
    #[error("the message content is {actual} bytes, the Matrix event ceiling leaves {limit}")]
    MessageTooLong {
        /// Ceiling in bytes ([`MAX_CONTENT_BYTES`](crate::matrix_out::MAX_CONTENT_BYTES)).
        limit: usize,
        /// Actual content size in bytes.
        actual: usize,
    },

    /// The upload is larger than the configured media ceiling.
    #[error("the upload is {actual} bytes, the configured media limit is {limit}")]
    MediaTooLarge {
        /// Ceiling in bytes.
        limit: u64,
        /// Actual size in bytes.
        actual: u64,
    },

    /// The homeserver rate-limited the request. Nothing was applied; retry
    /// after `retry_after` (or on the caller's own schedule).
    #[error(
        "the homeserver rate-limited the request ({errcode}): {message}{suffix}",
        suffix = retry_after_suffix(.retry_after.as_ref())
    )]
    RateLimited {
        /// How long the homeserver asked the client to wait, when it said so.
        retry_after: Option<Duration>,
        /// The Matrix error code, usually `M_LIMIT_EXCEEDED`.
        errcode: String,
        /// The homeserver's own message.
        message: String,
    },

    /// The homeserver refused the request and will refuse it again. Nothing was
    /// applied.
    #[error("the homeserver rejected the request with {status} ({errcode}): {message}")]
    Rejected {
        /// HTTP status the homeserver answered with.
        status: u16,
        /// The Matrix error code, when the body carried one.
        errcode: String,
        /// The homeserver's own message.
        message: String,
    },

    /// The homeserver failed (`5xx`) on a request that is safe to repeat, and
    /// the client ran out of attempts. Nothing is known to be applied.
    #[error("the homeserver answered {status} ({errcode}) after {attempts} attempts: {message}")]
    Server {
        /// HTTP status of the last attempt.
        status: u16,
        /// The Matrix error code, when the body carried one.
        errcode: String,
        /// The homeserver's own message.
        message: String,
        /// How many attempts were made.
        attempts: u32,
    },

    /// A write may or may not have been applied. Retrying blindly duplicates;
    /// repeating the *same* transaction id does not.
    #[error(
        "the write may or may not have been applied after {attempts} attempts: {reason}{suffix}",
        suffix = txn_id_suffix(.txn_id.as_ref())
    )]
    Ambiguous {
        /// The transaction id of the send, when the operation had one. Use it to
        /// repeat the send idempotently.
        txn_id: Option<TxnId>,
        /// How many attempts were made.
        attempts: u32,
        /// Why the outcome is unknown.
        reason: String,
    },

    /// No HTTP response was produced on any attempt. Only returned when nothing
    /// could have been applied (a connection that was never established), so it
    /// is retryable even for a non-idempotent request.
    #[error("the transport failed for all {attempts} attempts: {reason}")]
    Transport {
        /// How many attempts were made.
        attempts: u32,
        /// The transport's own message from the last attempt.
        reason: String,
    },

    /// A test transport was asked for a response the test never scripted. This
    /// is a broken test fixture, not a homeserver or network outcome.
    #[error("the test transport was not scripted for this request: {reason}")]
    TestHarness {
        /// Which request went unanswered.
        reason: String,
    },

    /// A `2xx` arrived but the body was not the JSON shape the leg expects.
    #[error("cannot decode the {what} response: {reason}")]
    Decode {
        /// Which response was being decoded.
        what: &'static str,
        /// The parse error.
        reason: String,
    },
}

fn retry_after_suffix(retry_after: Option<&Duration>) -> String {
    retry_after.map_or_else(String::new, |wait| {
        format!(" — retry after {} ms", wait.as_millis())
    })
}

fn txn_id_suffix(txn_id: Option<&TxnId>) -> String {
    txn_id.map_or_else(String::new, |txn| {
        format!(" (repeat transaction id `{txn}` to send it at most once)")
    })
}

impl OutboundError {
    /// Whether the same request could succeed if it were retried unchanged.
    ///
    /// `true` for [`OutboundError::RateLimited`], [`OutboundError::Server`] and
    /// [`OutboundError::Transport`] — three cases where the request provably was
    /// not applied. `false` for an ambiguous write: retrying it blindly is how a
    /// message gets posted twice.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::OutboundError;
    ///
    /// let limited = OutboundError::RateLimited {
    ///     retry_after: Some(Duration::from_secs(1)),
    ///     errcode: "M_LIMIT_EXCEEDED".into(),
    ///     message: "slow down".into(),
    /// };
    /// assert!(limited.is_retryable());
    /// assert!(!limited.is_ambiguous());
    /// ```
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::Server { .. } | Self::Transport { .. }
        )
    }

    /// Whether the request may have been applied.
    ///
    /// Only an [`OutboundError::Ambiguous`] is ambiguous. A caller must not
    /// treat it as a failure: the message may be in the room already.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::{OutboundError, TxnId};
    ///
    /// let error = OutboundError::Ambiguous {
    ///     txn_id: TxnId::new("t1").ok(),
    ///     attempts: 3,
    ///     reason: "read timeout".into(),
    /// };
    /// assert!(error.is_ambiguous());
    /// assert!(!error.is_retryable());
    /// assert!(error.to_string().contains("t1"));
    /// ```
    #[must_use]
    pub const fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous { .. })
    }

    /// The transaction id of an ambiguous write, when it had one.
    ///
    /// Repeat the send with this id instead of minting a new one: Matrix
    /// de-duplicates on it, so the second request either creates the event or
    /// returns the id of the one that already landed.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::{OutboundError, TxnId};
    ///
    /// let txn = TxnId::new("t1")?;
    /// let error = OutboundError::Ambiguous { txn_id: Some(txn.clone()), attempts: 1, reason: "timeout".into() };
    /// assert_eq!(error.txn_id(), Some(&txn));
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn txn_id(&self) -> Option<&TxnId> {
        match self {
            Self::Ambiguous { txn_id, .. } => txn_id.as_ref(),
            _ => None,
        }
    }

    /// Whether the failure is terminal for this request: the caller must change
    /// the request, credentials or permissions before trying again.
    ///
    /// A `true` answer means the same call will always fail with the same error
    /// until the argument changes.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::OutboundError;
    ///
    /// assert!(OutboundError::InvalidRequest { reason: "x".into() }.is_client_error());
    /// assert!(OutboundError::Rejected {
    ///     status: 403,
    ///     errcode: "M_FORBIDDEN".into(),
    ///     message: "not in room".into(),
    /// }
    /// .is_client_error());
    /// assert!(!OutboundError::Transport { attempts: 1, reason: "dns".into() }.is_client_error());
    /// ```
    #[must_use]
    pub const fn is_client_error(&self) -> bool {
        matches!(
            self,
            Self::InvalidConfig { .. }
                | Self::InvalidTxnId { .. }
                | Self::InvalidMxcUri { .. }
                | Self::InvalidRequest { .. }
                | Self::MessageTooLong { .. }
                | Self::MediaTooLarge { .. }
                | Self::Rejected { .. }
        )
    }
}

/// Convenience alias for the outbound [`enum@OutboundError`].
///
/// ```
/// use messenger_webhook::matrix_out::OutboundResult;
///
/// fn always_fine() -> OutboundResult<u8> {
///     Ok(1)
/// }
/// assert_eq!(always_fine().ok(), Some(1));
/// ```
pub type OutboundResult<T> = Result<T, OutboundError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The client-side half of the variant table.
    #[test]
    fn every_client_side_variant_reports_its_classification() {
        let cases: Vec<(OutboundError, bool, bool, bool, String)> = vec![
            (
                OutboundError::InvalidConfig {
                    reason: "no scheme".into(),
                },
                false,
                false,
                true,
                "invalid Matrix client configuration: no scheme".into(),
            ),
            (
                OutboundError::InvalidTxnId {
                    reason: "empty".into(),
                },
                false,
                false,
                true,
                "invalid transaction id: empty".into(),
            ),
            (
                OutboundError::InvalidMxcUri {
                    reason: "no mxc".into(),
                },
                false,
                false,
                true,
                "invalid Matrix content URI: no mxc".into(),
            ),
            (
                OutboundError::InvalidRequest {
                    reason: "empty room".into(),
                },
                false,
                false,
                true,
                "cannot send this request: empty room".into(),
            ),
            (
                OutboundError::MessageTooLong {
                    limit: 10,
                    actual: 11,
                },
                false,
                false,
                true,
                "the message content is 11 bytes, the Matrix event ceiling leaves 10".into(),
            ),
            (
                OutboundError::MediaTooLarge {
                    limit: 5,
                    actual: 6,
                },
                false,
                false,
                true,
                "the upload is 6 bytes, the configured media limit is 5".into(),
            ),
        ];

        for (error, retryable, ambiguous, client_error, text) in cases {
            assert_eq!(error.is_retryable(), retryable, "{error:?}");
            assert_eq!(error.is_ambiguous(), ambiguous, "{error:?}");
            assert_eq!(error.is_client_error(), client_error, "{error:?}");
            assert_eq!(error.to_string(), text, "{error:?}");
        }
    }

    /// Client-error rows among the delivery variants: the homeserver or
    /// homeserver-adjacent layers told the caller to change the request.
    #[test]
    fn every_rejected_variant_reports_its_classification() {
        let cases: Vec<(OutboundError, bool, bool, bool, String)> = vec![
            (
                OutboundError::Rejected {
                    status: 400,
                    errcode: "M_UNKNOWN".into(),
                    message: "bad request".into(),
                },
                false,
                false,
                true,
                "the homeserver rejected the request with 400 (M_UNKNOWN): bad request".into(),
            ),
            (
                OutboundError::Rejected {
                    status: 403,
                    errcode: "M_FORBIDDEN".into(),
                    message: "not in room".into(),
                },
                false,
                false,
                true,
                "the homeserver rejected the request with 403 (M_FORBIDDEN): not in room".into(),
            ),
            (
                OutboundError::Rejected {
                    status: 404,
                    errcode: "M_NOT_FOUND".into(),
                    message: "no such room".into(),
                },
                false,
                false,
                true,
                "the homeserver rejected the request with 404 (M_NOT_FOUND): no such room".into(),
            ),
            (
                OutboundError::Rejected {
                    status: 413,
                    errcode: "M_TOO_LARGE".into(),
                    message: "too big".into(),
                },
                false,
                false,
                true,
                "the homeserver rejected the request with 413 (M_TOO_LARGE): too big".into(),
            ),
        ];

        for (error, retryable, ambiguous, client_error, text) in cases {
            assert_eq!(error.is_retryable(), retryable, "{error:?}");
            assert_eq!(error.is_ambiguous(), ambiguous, "{error:?}");
            assert_eq!(error.is_client_error(), client_error, "{error:?}");
            assert_eq!(error.to_string(), text, "{error:?}");
        }
    }

    /// The non-rejected delivery variants: what the homeserver or the network
    /// did after the request had already been formed.
    #[test]
    fn every_non_rejected_delivery_variant_reports_its_classification() {
        let txn = TxnId::new("t1").expect("valid txn");
        let cases: Vec<(OutboundError, bool, bool, bool, String)> = vec![
            (
                OutboundError::RateLimited {
                    retry_after: Some(Duration::from_millis(1500)),
                    errcode: "M_LIMIT_EXCEEDED".into(),
                    message: "slow down".into(),
                },
                true,
                false,
                false,
                "the homeserver rate-limited the request (M_LIMIT_EXCEEDED): slow down \
                 — retry after 1500 ms"
                    .into(),
            ),
            (
                OutboundError::RateLimited {
                    retry_after: None,
                    errcode: "M_LIMIT_EXCEEDED".into(),
                    message: "slow down".into(),
                },
                true,
                false,
                false,
                "the homeserver rate-limited the request (M_LIMIT_EXCEEDED): slow down".into(),
            ),
            (
                OutboundError::Server {
                    status: 503,
                    errcode: "M_UNKNOWN".into(),
                    message: "down".into(),
                    attempts: 3,
                },
                true,
                false,
                false,
                "the homeserver answered 503 (M_UNKNOWN) after 3 attempts: down".into(),
            ),
            (
                OutboundError::Ambiguous {
                    txn_id: Some(txn.clone()),
                    attempts: 2,
                    reason: "read timeout".into(),
                },
                false,
                true,
                false,
                "the write may or may not have been applied after 2 attempts: read timeout \
                 (repeat transaction id `t1` to send it at most once)"
                    .into(),
            ),
            (
                OutboundError::Ambiguous {
                    txn_id: None,
                    attempts: 1,
                    reason: "read timeout".into(),
                },
                false,
                true,
                false,
                "the write may or may not have been applied after 1 attempts: read timeout".into(),
            ),
            (
                OutboundError::Transport {
                    attempts: 2,
                    reason: "dns".into(),
                },
                true,
                false,
                false,
                "the transport failed for all 2 attempts: dns".into(),
            ),
        ];

        for (error, retryable, ambiguous, client_error, text) in cases {
            assert_eq!(error.is_retryable(), retryable, "{error:?}");
            assert_eq!(error.is_ambiguous(), ambiguous, "{error:?}");
            assert_eq!(error.is_client_error(), client_error, "{error:?}");
            assert_eq!(error.to_string(), text, "{error:?}");
        }
    }

    /// Test harness failures are neither retryable nor ambiguous: they mean the
    /// test forgot to script a call, not that a delivery failed.
    #[test]
    fn test_harness_errors_report_their_classification() {
        let error = OutboundError::TestHarness {
            reason: "PUT https://m.example/a ran out of scripted responses".into(),
        };

        assert!(!error.is_retryable());
        assert!(!error.is_ambiguous());
        assert!(!error.is_client_error());
        assert_eq!(
            error.to_string(),
            "the test transport was not scripted for this request: PUT \
             https://m.example/a ran out of scripted responses"
        );
    }

    /// The remaining delivery variants: transport failures and response-body
    /// decode failures.
    #[test]
    fn transport_and_decode_variants_report_their_classification() {
        let cases: Vec<(OutboundError, bool, bool, bool, String)> = vec![(
            OutboundError::Decode {
                what: "whoami",
                reason: "missing user_id".into(),
            },
            false,
            false,
            false,
            "cannot decode the whoami response: missing user_id".into(),
        )];

        for (error, retryable, ambiguous, client_error, text) in cases {
            assert_eq!(error.is_retryable(), retryable, "{error:?}");
            assert_eq!(error.is_ambiguous(), ambiguous, "{error:?}");
            assert_eq!(error.is_client_error(), client_error, "{error:?}");
            assert_eq!(error.to_string(), text, "{error:?}");
        }
    }

    #[test]
    fn only_an_ambiguous_error_carries_or_reports_a_txn_id() {
        let txn = TxnId::new("t1").expect("valid txn");
        let ambiguous = OutboundError::Ambiguous {
            txn_id: Some(txn.clone()),
            attempts: 1,
            reason: "timeout".into(),
        };
        assert_eq!(ambiguous.txn_id(), Some(&txn));
        assert_eq!(
            OutboundError::Ambiguous {
                txn_id: None,
                attempts: 1,
                reason: "timeout".into()
            }
            .txn_id(),
            None
        );
        assert_eq!(
            OutboundError::Transport {
                attempts: 1,
                reason: "dns".into()
            }
            .txn_id(),
            None
        );
    }

    #[test]
    fn no_error_is_both_retryable_and_ambiguous() {
        let errors = [
            OutboundError::Ambiguous {
                txn_id: None,
                attempts: 1,
                reason: "x".into(),
            },
            OutboundError::RateLimited {
                retry_after: None,
                errcode: "M_LIMIT_EXCEEDED".into(),
                message: "x".into(),
            },
            OutboundError::Rejected {
                status: 400,
                errcode: "M_UNKNOWN".into(),
                message: "x".into(),
            },
        ];
        for error in errors {
            assert!(!(error.is_retryable() && error.is_ambiguous()), "{error:?}");
        }
    }

    #[test]
    fn the_error_is_a_std_error() {
        fn assert_error<E: std::error::Error>(_: &E) {}
        assert_error(&OutboundError::InvalidRequest { reason: "x".into() });
    }
}
