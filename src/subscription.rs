//! Meta's webhook-subscription handshake.
//!
//! Before Meta posts a single event it asks the endpoint to prove it is the
//! intended receiver: a `GET` on the webhook URL carrying `hub.mode`,
//! `hub.challenge` and `hub.verify_token`. The endpoint has to compare the
//! token and echo the challenge back. Meta's *Getting Started* page states the
//! obligation in two steps:
//!
//! > Whenever your endpoint receives a verification request, it must:
//! > Verify that the `hub.verify_token` value matches the string you set in the
//! > **Verify Token** field when you configure the Webhooks product in your App
//! > Dashboard […] Respond with the `hub.challenge` value.
//!
//! The HMAC check in [`verify_meta`](crate::signature::verify_meta) covers the
//! *event* requests; this module covers the subscription request that comes
//! before the first one. It is not optional: an endpoint that cannot answer the
//! handshake can never be subscribed, and a caller left to hand-roll it usually
//! compares the token with `==` (timing leak) or echoes whatever it is given
//! (reflection).
//!
//! The query string is parsed with [`form_urlencoded`], the parser for
//! `application/x-www-form-urlencoded`, rather than by splitting on `&`: `+` is
//! a space there, `%` needs decoding, and a key may legally appear twice —
//! three behaviours worth getting from a maintained crate instead of a
//! hand-rolled loop.
//!
//! ```
//! use messenger_webhook::{subscription::meta_subscription_challenge, Error, Secret};
//!
//! // The request Meta sends when the webhook URL is saved in the app dashboard.
//! let query = "hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token=meatyhamhock";
//! let challenge = meta_subscription_challenge(&Secret::new("meatyhamhock"), query)?;
//! assert_eq!(challenge, "1158201444");
//!
//! // Answer with the challenge — and nothing else — as `text/plain`.
//! // A different token is rejected with a typed error.
//! let error = meta_subscription_challenge(&Secret::new("wrong"), query).unwrap_err();
//! assert!(matches!(error, Error::BadSignature { .. }));
//! # Ok::<(), messenger_webhook::Error>(())
//! ```

use form_urlencoded;

use crate::error::{Error, Result};
use crate::model::Channel;
use crate::secret::Secret;

/// Query parameter that carries the subscription mode.
///
/// ```
/// use messenger_webhook::subscription::META_MODE_PARAM;
///
/// assert_eq!(META_MODE_PARAM, "hub.mode");
/// ```
pub const META_MODE_PARAM: &str = "hub.mode";

/// Query parameter that carries the value the endpoint has to echo back.
///
/// ```
/// use messenger_webhook::subscription::META_CHALLENGE_PARAM;
///
/// assert_eq!(META_CHALLENGE_PARAM, "hub.challenge");
/// ```
pub const META_CHALLENGE_PARAM: &str = "hub.challenge";

/// Query parameter that carries the token entered in the app dashboard.
///
/// ```
/// use messenger_webhook::subscription::META_VERIFY_TOKEN_PARAM;
///
/// assert_eq!(META_VERIFY_TOKEN_PARAM, "hub.verify_token");
/// ```
pub const META_VERIFY_TOKEN_PARAM: &str = "hub.verify_token";

/// The only value [`META_MODE_PARAM`] may carry.
///
/// Meta's table says of `hub.mode`:
///
/// > This value will always be set to `subscribe`.
///
/// ```
/// use messenger_webhook::subscription::META_SUBSCRIBE_MODE;
///
/// assert_eq!(META_SUBSCRIBE_MODE, "subscribe");
/// ```
pub const META_SUBSCRIBE_MODE: &str = "subscribe";

/// Longest challenge this crate echoes back, in bytes.
///
/// Meta documents the challenge as an integer, so any real value is a handful
/// of digits; the ceiling only exists because the value is reflected into the
/// response and a reflected value must be bounded.
///
/// ```
/// use messenger_webhook::subscription::MAX_CHALLENGE_BYTES;
///
/// assert_eq!(MAX_CHALLENGE_BYTES, 128);
/// ```
pub const MAX_CHALLENGE_BYTES: usize = 128;

/// The spelling PHP forces on the same parameters: `.` becomes `_`.
const PHP_MODE_PARAM: &str = "hub_mode";

/// The PHP spelling of [`META_CHALLENGE_PARAM`].
const PHP_CHALLENGE_PARAM: &str = "hub_challenge";

/// The PHP spelling of [`META_VERIFY_TOKEN_PARAM`].
const PHP_VERIFY_TOKEN_PARAM: &str = "hub_verify_token";

/// Validates Meta's subscription handshake and returns the challenge to echo.
///
/// Pass the raw query string (everything after `?` in the `GET` URL) and the
/// token that was entered in the app dashboard. On success the caller answers
/// `200` with the returned value as its body and `text/plain` as its content
/// type; every error means the request is not from Meta and deserves `403`.
///
/// ```
/// use messenger_webhook::{subscription::meta_subscription_challenge, Secret};
///
/// let secret = Secret::new("meatyhamhock");
/// let query = "hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token=meatyhamhock";
///
/// assert_eq!(meta_subscription_challenge(&secret, query)?, "1158201444");
///
/// // A missing token is rejected before anything is compared.
/// let query = "hub.mode=subscribe&hub.challenge=1158201444";
/// assert!(meta_subscription_challenge(&secret, query).is_err());
/// # Ok::<(), messenger_webhook::Error>(())
/// ```
///
/// # Errors
///
/// - [`Error::MissingSecret`] when `secret` is empty: comparing against an
///   unset token would make the handshake succeed for anyone, so the adapter
///   fails closed instead of accepting the request.
/// - [`Error::MalformedVerificationRequest`] when a parameter is missing, is
///   present twice, is not a single `subscribe` in `hub.mode`, or when the
///   challenge is empty, longer than [`MAX_CHALLENGE_BYTES`] or not printable, non-space
///   ASCII — it is reflected into the response, so a control byte in it would
///   be a response-splitting bug in the caller.
/// - [`Error::BadSignature`] when the token does not match. The comparison is
///   constant time, and the raw value never reaches the error message.
pub fn meta_subscription_challenge(secret: &Secret, query: &str) -> Result<String> {
    if secret.is_empty() {
        return Err(Error::MissingSecret {
            channel: Channel::WhatsApp,
        });
    }

    let mut mode = None;
    let mut challenge = None;
    let mut verify_token = None;

    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        let slot = match key.as_ref() {
            META_MODE_PARAM | PHP_MODE_PARAM => &mut mode,
            META_CHALLENGE_PARAM | PHP_CHALLENGE_PARAM => &mut challenge,
            META_VERIFY_TOKEN_PARAM | PHP_VERIFY_TOKEN_PARAM => &mut verify_token,
            // Unknown parameters are ignored: Meta adds fields to this request
            // without warning, and an extra one is not evidence of forgery.
            _ => continue,
        };
        if slot.replace(value.into_owned()).is_some() {
            // Two values for one parameter is ambiguous: which of them was
            // meant to be verified? Reject instead of picking one.
            return Err(malformed("a parameter appears more than once"));
        }
    }

    let mode = mode.ok_or_else(|| malformed("no hub.mode parameter"))?;
    if !mode.eq_ignore_ascii_case(META_SUBSCRIBE_MODE) {
        return Err(malformed("hub.mode is not subscribe"));
    }

    let verify_token = verify_token.ok_or_else(|| malformed("no hub.verify_token parameter"))?;
    if !secret.ct_eq(verify_token.as_bytes()) {
        return Err(Error::BadSignature {
            channel: Channel::WhatsApp,
        });
    }

    let challenge = challenge.ok_or_else(|| malformed("no hub.challenge parameter"))?;
    if challenge.is_empty() {
        return Err(malformed("hub.challenge is empty"));
    }
    if challenge.len() > MAX_CHALLENGE_BYTES {
        return Err(malformed("hub.challenge is over the length limit"));
    }
    // Space is excluded as well: the challenge is documented as an integer, so
    // a space in it means the query was mangled (a `+` or a stray `%20`), and a
    // whitespace-only value is never worth reflecting.
    if !challenge.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(malformed("hub.challenge is not printable ASCII"));
    }

    Ok(challenge)
}

/// Builds the typed error for a request that does not have the documented shape.
///
/// The reason names the parameter and the defect, never the received value:
/// the value is attacker-controlled and would end up in a log line.
fn malformed(reason: &str) -> Error {
    Error::MalformedVerificationRequest {
        channel: Channel::WhatsApp,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample request from Meta's *Getting Started* page.
    const DOCS_QUERY: &str =
        "hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token=meatyhamhock";

    #[test]
    fn the_documented_request_is_accepted() {
        let challenge = meta_subscription_challenge(&Secret::new("meatyhamhock"), DOCS_QUERY)
            .expect("challenge");
        assert_eq!(challenge, "1158201444");
    }

    #[test]
    fn the_challenge_is_the_only_thing_returned() {
        // Nothing from the query is concatenated into the answer.
        let query = "hub.mode=subscribe&hub.challenge=42&hub.verify_token=t";
        let challenge = meta_subscription_challenge(&Secret::new("t"), query).expect("challenge");
        assert_eq!(challenge, "42");
        assert!(!challenge.contains('t'));
    }
}
