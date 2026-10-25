//! An Axum webhook handler — the deployment shape this crate is built for.
//!
//! The example shows the three things a handler has to get right: hand the **raw** body
//! to [`Adapter::verify`] before anything parses it, map the framework's header map onto the
//! crate's framework-neutral [`Headers`], and fail closed on anything unverifiable.
//!
//! ```sh
//! WA_APP_SECRET=dev-only-secret cargo run --example axum_handler
//! ```
//!
//! Then, in a second shell (prints `200` for a valid signature, `401` otherwise):
//!
//! ```sh
//! body='{"entry":[]}'
//! sig="sha256=$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$WA_APP_SECRET" | sed 's/^.* //')"
//! curl -sS -D - -o /dev/null -X POST localhost:3000/webhook/whatsapp \
//!   -H "X-Hub-Signature-256: $sig" --data "$body"
//! ```

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use messenger_webhook::{Adapter, Headers, NormalizedMessage, WhatsAppAdapter};

/// Adapters are `Send + Sync` and stateless, so an `Arc` is the whole shared
/// state — no `Mutex`, and a request never waits for another one.
type AppState = Arc<WhatsAppAdapter>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::env::var("WA_APP_SECRET").unwrap_or_else(|_| "dev-only-secret".to_owned());
    let state: AppState = Arc::new(WhatsAppAdapter::new(secret));

    let app = Router::new()
        .route("/webhook/whatsapp", post(handle))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    println!(
        "listening on http://{}/webhook/whatsapp",
        listener.local_addr()?
    );
    axum::serve(listener, app).await?;

    Ok(())
}

/// Verifies, normalises, and hands the events to the application.
///
/// The body arrives as [`Bytes`], i.e. exactly the bytes on the wire: a handler that let Axum
/// deserialise first would lose the data the signature covers.
async fn handle(State(adapter): State<AppState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let headers = neutral_headers(&headers);
    if let Err(error) = adapter.verify(&headers, &body) {
        // Fail closed: an unverifiable body never reaches the parser, and the
        // reason stays in the log instead of the response.
        eprintln!("rejected webhook: {error}");
        return StatusCode::UNAUTHORIZED;
    }

    match adapter.adapt(&body) {
        Ok(events) => {
            for message in &events {
                handle_event(message);
            }
            // Meta retries on any non-2xx, so acknowledge even an event this
            // application has no use for.
            StatusCode::OK
        }
        Err(error) => {
            eprintln!("unparsable webhook: {error}");
            StatusCode::BAD_REQUEST
        }
    }
}

/// The application's own step: idempotent on `external_id`, which is the
/// provider's event id (`wamid.…`, Telegram `message_id`, Matrix `event_id`).
fn handle_event(message: &NormalizedMessage) {
    println!(
        "{} {} in {}: {}",
        message.channel,
        message.external_id,
        message.conversation_id,
        message.text.as_deref().unwrap_or("<no text>"),
    );
}

/// Copies the request headers into the crate's case-insensitive set, skipping
/// values that are not valid UTF-8 (a signature header never is anything else).
fn neutral_headers(map: &HeaderMap) -> Headers {
    let mut headers = Headers::new();
    for (name, value) in map {
        if let Ok(value) = value.to_str() {
            headers.insert(name.as_str(), value);
        }
    }
    headers
}
