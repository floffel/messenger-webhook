//! The outbound error surface: fail closed, and say whether a retry is safe.
//!
//! Three questions decide what a caller may do next, and every variant answers
//! exactly one way:
//!
//! * **retryable** ([`OutboundError::is_retryable`]) — repeating the same
//!   request is safe and it may succeed later. Requeue it.
//! * **ambiguous** ([`OutboundError::is_ambiguous`]) — the homeserver may have
//!   applied the request. Do **not** mint a new transaction id and send again:
//!   repeat the *same* one ([`MatrixClient::send_text_with_txn`]) or reconcile
//!   by reading the room.
//! * **terminal** — neither retryable nor ambiguous: the caller must change the
//!   request, configuration or fixture before trying again. This explicit
//!   bucket holds the request/configuration validation variants, a homeserver
//!   [`OutboundError::Rejected`], and local [`OutboundError::DecodeRead`] or
//!   [`OutboundError::TestHarness`] failures.
//!
//! [`MatrixClient::send_text_with_txn`]: crate::matrix_out::MatrixClient::send_text_with_txn

use std::time::Duration;

use thiserror::Error;

use crate::matrix_out::retry::Operation;
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

    /// The request itself cannot be addressed or expressed: an empty room id,
    /// an id of the wrong shape, an empty message body, an unusable media type,
    /// or a header HTTP cannot carry.
    #[error("cannot send this request: {reason}")]
    InvalidRequest {
        /// What has to change before the request is sendable.
        reason: String,
    },

    /// The per-room ordering queue is already full, so no HTTP request was
    /// attempted.
    #[error("the room ordering queue for {room_id} already has {limit} waiters")]
    GateOverflow {
        /// The room whose ordering queue is full.
        room_id: String,
        /// Maximum number of queued callers admitted per room.
        limit: usize,
    },

    /// The per-room ordering gate did not become available before the local
    /// queue timeout elapsed, so no HTTP request was attempted.
    #[error("timed out after {waited:?} waiting for the ordering gate for {room_id}")]
    GateTimeout {
        /// The room whose ordering gate timed out.
        room_id: String,
        /// How long the caller waited before giving up.
        waited: Duration,
    },

    /// Per-room ordering could not be preserved because the gate map was full
    /// and [`OrderingSaturation::Error`] was configured.
    ///
    /// Nothing was sent. Retry after another room has finished and released its
    /// gate, or choose [`OrderingSaturation::Backpressure`] to wait instead.
    ///
    /// [`OrderingSaturation::Error`]: crate::matrix_out::OrderingSaturation::Error
    /// [`OrderingSaturation::Backpressure`]: crate::matrix_out::OrderingSaturation::Backpressure
    #[error(
        "per-room ordering is saturated at {capacity} active rooms; refusing unordered send to {room_id}"
    )]
    OrderingLost {
        /// The validated room id whose ordering guarantee was requested.
        room_id: String,
        /// The configured per-room gate capacity.
        capacity: usize,
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
    /// the client ran out of attempts. This variant is retryable only while the
    /// carried operation and transaction id say repeating it is safe; an
    /// uncertain write is reported as [`OutboundError::Ambiguous`].
    #[error("the homeserver answered {status} ({errcode}) after {attempts} attempts: {message}")]
    Server {
        /// The operation whose retry-safety this error describes.
        operation: Operation,
        /// The transaction id of the request, when it had one.
        txn_id: Option<TxnId>,
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

    /// A `2xx` arrived for a write, but the body was not the JSON shape the leg
    /// expects. The homeserver accepted the request, so the write may have been
    /// applied; repeat with the same transaction id when one is present.
    #[error(
        "the homeserver accepted the {what} but answered unreadably: {reason}{suffix}",
        suffix = txn_id_suffix(.txn_id.as_ref())
    )]
    DecodeAfterWrite {
        /// Which response was being decoded.
        what: &'static str,
        /// The transaction id of the write, when the operation had one.
        txn_id: Option<TxnId>,
        /// The parse error.
        reason: String,
    },

    /// A `2xx` arrived for a read, but the body was not the JSON shape the leg
    /// expects. Nothing was applied.
    #[error("cannot decode the {what} response: {reason}")]
    DecodeRead {
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
    /// `true` for [`OutboundError::RateLimited`], [`OutboundError::Transport`],
    /// [`OutboundError::GateOverflow`], [`OutboundError::GateTimeout`] and
    /// [`OutboundError::OrderingLost`], and [`OutboundError::Server`] only when
    /// the server error belongs to a repeatable operation with no transaction
    /// id. `false` for an ambiguous write: retrying it blindly is how a message
    /// gets posted twice.
    ///
    /// ```
    /// use std::time::Duration;
    /// use messenger_webhook::matrix_out::{Operation, OutboundError};
    ///
    /// let limited = OutboundError::RateLimited {
    ///     retry_after: Some(Duration::from_secs(1)),
    ///     errcode: "M_LIMIT_EXCEEDED".into(),
    ///     message: "slow down".into(),
    /// };
    /// assert!(limited.is_retryable());
    /// assert!(!limited.is_ambiguous());
    ///
    /// let server = OutboundError::Server {
    ///     operation: Operation::Read,
    ///     txn_id: None,
    ///     status: 503,
    ///     errcode: "M_UNKNOWN".into(),
    ///     message: "down".into(),
    ///     attempts: 3,
    /// };
    /// assert!(server.is_retryable());
    ///
    /// let uncertain_upload = OutboundError::Server {
    ///     operation: Operation::NonIdempotent,
    ///     txn_id: None,
    ///     status: 503,
    ///     errcode: "M_UNKNOWN".into(),
    ///     message: "down".into(),
    ///     attempts: 3,
    /// };
    /// assert!(!uncertain_upload.is_retryable());
    /// assert!(uncertain_upload.is_ambiguous());
    ///
    /// let keyless_send = OutboundError::Server {
    ///     operation: Operation::Idempotent,
    ///     txn_id: None,
    ///     status: 503,
    ///     errcode: "M_UNKNOWN".into(),
    ///     message: "down".into(),
    ///     attempts: 3,
    /// };
    /// assert!(!keyless_send.is_retryable());
    /// assert!(keyless_send.is_ambiguous());
    /// ```
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. }
            | Self::Transport { .. }
            | Self::GateOverflow { .. }
            | Self::GateTimeout { .. }
            | Self::OrderingLost { .. } => true,
            Self::Server {
                operation, txn_id, ..
            } => txn_id.is_none() && operation.is_repeatable_with(txn_id.as_ref()),
            _ => false,
        }
    }

    /// Whether the request may have been applied.
    ///
    /// [`OutboundError::Ambiguous`], [`OutboundError::DecodeAfterWrite`] and
    /// [`OutboundError::Server`] are ambiguous when their carried operation or
    /// transaction id says the request may have been applied. A caller must not
    /// treat them as plain failures: the message may be in the room already.
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
    ///
    /// let decoded = OutboundError::DecodeAfterWrite {
    ///     what: "event send",
    ///     txn_id: TxnId::new("t2").ok(),
    ///     reason: "bad body".into(),
    /// };
    /// assert!(decoded.is_ambiguous());
    /// assert!(decoded.to_string().contains("t2"));
    /// ```
    #[must_use]
    pub const fn is_ambiguous(&self) -> bool {
        match self {
            Self::Ambiguous { .. } | Self::DecodeAfterWrite { .. } => true,
            Self::Server {
                operation, txn_id, ..
            } => txn_id.is_some() || !operation.is_repeatable_with(txn_id.as_ref()),
            _ => false,
        }
    }

    /// The transaction id of an uncertain write, when it had one.
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
    ///
    /// let decoded = OutboundError::DecodeAfterWrite {
    ///     what: "event send",
    ///     txn_id: Some(txn.clone()),
    ///     reason: "bad body".into(),
    /// };
    /// assert_eq!(decoded.txn_id(), Some(&txn));
    /// # Ok::<(), messenger_webhook::matrix_out::OutboundError>(())
    /// ```
    #[must_use]
    pub fn txn_id(&self) -> Option<&TxnId> {
        match self {
            Self::Ambiguous { txn_id, .. }
            | Self::DecodeAfterWrite { txn_id, .. }
            | Self::Server { txn_id, .. } => txn_id.as_ref(),
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
                    operation: Operation::Read,
                    txn_id: None,
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
            (
                OutboundError::OrderingLost {
                    room_id: "!busy:m.example".into(),
                    capacity: 2,
                },
                true,
                false,
                false,
                "per-room ordering is saturated at 2 active rooms; refusing unordered send to !busy:m.example".into(),
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
            OutboundError::DecodeRead {
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
        let decoded = OutboundError::DecodeAfterWrite {
            what: "event send",
            txn_id: Some(txn.clone()),
            reason: "bad body".into(),
        };
        assert_eq!(decoded.txn_id(), Some(&txn));
        assert_eq!(
            OutboundError::DecodeAfterWrite {
                what: "media upload",
                txn_id: None,
                reason: "bad body".into()
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
    fn server_errors_keep_their_retry_safety_from_operation_and_txn() {
        let txn = TxnId::new("t1").expect("valid txn");
        let base = OutboundError::Server {
            operation: Operation::Idempotent,
            txn_id: None,
            status: 503,
            errcode: "M_UNKNOWN".into(),
            message: "down".into(),
            attempts: 3,
        };
        assert!(!base.is_retryable());
        assert!(base.is_ambiguous());

        let txn_keyed = OutboundError::Server {
            operation: Operation::Idempotent,
            txn_id: Some(txn.clone()),
            status: 503,
            errcode: "M_UNKNOWN".into(),
            message: "down".into(),
            attempts: 3,
        };
        assert!(!txn_keyed.is_retryable());
        assert!(txn_keyed.is_ambiguous());
        assert_eq!(txn_keyed.txn_id(), Some(&txn));

        let state_setting = OutboundError::Server {
            operation: Operation::StateSetting,
            txn_id: None,
            status: 503,
            errcode: "M_UNKNOWN".into(),
            message: "down".into(),
            attempts: 3,
        };
        assert!(state_setting.is_retryable());
        assert!(!state_setting.is_ambiguous());

        let non_idempotent = OutboundError::Server {
            operation: Operation::NonIdempotent,
            txn_id: None,
            status: 503,
            errcode: "M_UNKNOWN".into(),
            message: "down".into(),
            attempts: 3,
        };
        assert!(!non_idempotent.is_retryable());
        assert!(non_idempotent.is_ambiguous());
        assert_eq!(non_idempotent.txn_id(), None);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Classification {
        Retryable,
        Ambiguous,
        Terminal,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ExpectedClassification {
        classification: Classification,
        client_error: bool,
    }

    impl ExpectedClassification {
        const fn retryable(self) -> bool {
            matches!(self.classification, Classification::Retryable)
        }

        const fn ambiguous(self) -> bool {
            matches!(self.classification, Classification::Ambiguous)
        }
    }

    fn expected_classification(error: &OutboundError) -> ExpectedClassification {
        use Classification::{Ambiguous, Retryable, Terminal};

        match error {
            OutboundError::InvalidConfig { .. }
            | OutboundError::InvalidTxnId { .. }
            | OutboundError::InvalidMxcUri { .. }
            | OutboundError::InvalidRequest { .. }
            | OutboundError::MessageTooLong { .. }
            | OutboundError::MediaTooLarge { .. }
            | OutboundError::Rejected { .. } => ExpectedClassification {
                classification: Terminal,
                client_error: true,
            },
            OutboundError::RateLimited { .. }
            | OutboundError::Transport { .. }
            | OutboundError::GateOverflow { .. }
            | OutboundError::GateTimeout { .. }
            | OutboundError::OrderingLost { .. } => ExpectedClassification {
                classification: Retryable,
                client_error: false,
            },
            OutboundError::Server {
                operation, txn_id, ..
            } if txn_id.is_none() && operation.is_repeatable_with(txn_id.as_ref()) => {
                ExpectedClassification {
                    classification: Retryable,
                    client_error: false,
                }
            }
            OutboundError::Server { .. }
            | OutboundError::Ambiguous { .. }
            | OutboundError::DecodeAfterWrite { .. } => ExpectedClassification {
                classification: Ambiguous,
                client_error: false,
            },
            OutboundError::TestHarness { .. } | OutboundError::DecodeRead { .. } => {
                ExpectedClassification {
                    classification: Terminal,
                    client_error: false,
                }
            }
        }
    }

    fn every_outbound_error() -> Vec<OutboundError> {
        let txn = TxnId::new("t1").expect("valid txn");
        vec![
            OutboundError::InvalidConfig {
                reason: "no scheme".into(),
            },
            OutboundError::InvalidTxnId {
                reason: "empty".into(),
            },
            OutboundError::InvalidMxcUri {
                reason: "no mxc".into(),
            },
            OutboundError::InvalidRequest {
                reason: "empty room".into(),
            },
            OutboundError::GateOverflow {
                room_id: "!room:x".into(),
                limit: 64,
            },
            OutboundError::GateTimeout {
                room_id: "!room:x".into(),
                waited: Duration::from_secs(30),
            },
            OutboundError::MessageTooLong {
                limit: 10,
                actual: 11,
            },
            OutboundError::MediaTooLarge {
                limit: 5,
                actual: 6,
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
            OutboundError::Server {
                operation: Operation::Read,
                txn_id: None,
                status: 503,
                errcode: "M_UNKNOWN".into(),
                message: "x".into(),
                attempts: 2,
            },
            OutboundError::Ambiguous {
                txn_id: Some(txn.clone()),
                attempts: 1,
                reason: "x".into(),
            },
            OutboundError::Transport {
                attempts: 1,
                reason: "x".into(),
            },
            OutboundError::OrderingLost {
                room_id: "!busy:m.example".into(),
                capacity: 1,
            },
            OutboundError::TestHarness { reason: "x".into() },
            OutboundError::DecodeAfterWrite {
                what: "event send",
                txn_id: Some(txn.clone()),
                reason: "x".into(),
            },
            OutboundError::DecodeRead {
                what: "whoami",
                reason: "x".into(),
            },
        ]
    }

    /// The classification is a finite contract: every variant answers each
    /// predicate explicitly, and adding a variant without updating this table
    /// fails to compile.
    #[test]
    fn every_error_variant_has_one_explicit_classification() {
        let errors = every_outbound_error();
        assert_eq!(errors.len(), 17, "the table must cover every variant");

        for error in errors {
            let expected = expected_classification(&error);
            assert_eq!(error.is_retryable(), expected.retryable(), "{error:?}");
            assert_eq!(error.is_ambiguous(), expected.ambiguous(), "{error:?}");
            assert_eq!(error.is_client_error(), expected.client_error, "{error:?}");
            assert!(!(error.is_retryable() && error.is_ambiguous()), "{error:?}");
        }
    }

    #[test]
    fn no_error_is_both_retryable_and_ambiguous() {
        for error in every_outbound_error() {
            assert!(!(error.is_retryable() && error.is_ambiguous()), "{error:?}");
        }
    }

    #[test]
    fn only_declared_predicate_combinations_are_allowed() {
        let allowed_combinations = [
            (false, false, false),
            (false, false, true),
            (false, true, false),
            (true, false, false),
        ];
        let errors = every_outbound_error();
        for (retryable, ambiguous, client_error) in [
            (false, false, false),
            (false, false, true),
            (false, true, false),
            (false, true, true),
            (true, false, false),
            (true, false, true),
            (true, true, false),
            (true, true, true),
        ] {
            let matching = errors.iter().any(|error| {
                error.is_retryable() == retryable
                    && error.is_ambiguous() == ambiguous
                    && error.is_client_error() == client_error
            });
            let allowed = allowed_combinations.contains(&(retryable, ambiguous, client_error));
            assert_eq!(
                matching, allowed,
                "retryable={retryable}, ambiguous={ambiguous}, client_error={client_error}"
            );
        }
    }

    #[test]
    fn the_error_is_a_std_error() {
        fn assert_error<E: std::error::Error>(_: &E) {}
        assert_error(&OutboundError::InvalidRequest { reason: "x".into() });
    }
}
