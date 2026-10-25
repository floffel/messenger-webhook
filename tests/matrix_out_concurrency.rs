//! 32 tasks over one client: no shared-state corruption, no torn sends, and the
//! per-room ordering guarantee actually holding under load.

#![cfg(feature = "matrix-out")]

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use messenger_webhook::matrix_out::transport::{
    BoxFuture, Request, Response, Transport, TransportError,
};
use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, StubTransport};

/// Tasks per test. Well above the core count, so any shared-state bug shows.
const TASKS: usize = 32;

fn client(transport: Arc<dyn Transport>) -> MatrixClient {
    MatrixClient::new(
        transport,
        ClientConfig::new("https://m.example")
            .expect("valid")
            .with_user_id("@bot:m.example"),
        "syt",
    )
    .expect("valid client")
}

/// A transport that holds each request open for `delay` and records how many
/// were in flight at once — in total and per room.
#[derive(Debug)]
struct SlowTransport {
    delay: Duration,
    live: Mutex<HashMap<String, u32>>,
    peak: Mutex<HashMap<String, u32>>,
    live_total: AtomicU32,
    peak_total: AtomicU32,
    calls: AtomicU32,
}

impl SlowTransport {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            delay,
            live: Mutex::new(HashMap::new()),
            peak: Mutex::new(HashMap::new()),
            live_total: AtomicU32::new(0),
            peak_total: AtomicU32::new(0),
            calls: AtomicU32::new(0),
        })
    }

    fn peak_for(&self, room_id: &str) -> u32 {
        let encoded = encode(room_id);
        *self
            .peak
            .lock()
            .expect("peak map")
            .get(&encoded)
            .unwrap_or(&0)
    }

    fn peak_total(&self) -> u32 {
        self.peak_total.load(Ordering::SeqCst)
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The same percent-encoding the client applies, so a test can key on a room.
fn encode(raw: &str) -> String {
    let mut out = String::new();
    for byte in raw.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(*byte));
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The room segment of a send URL, or `"?"` for a room-less leg.
fn room_of(url: &str) -> String {
    url.split("/rooms/")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("?")
        .to_owned()
}

impl Transport for SlowTransport {
    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Response, TransportError>> {
        Box::pin(async move {
            let room = room_of(request.url());
            {
                let mut live = self.live.lock().expect("live map");
                let count = live.entry(room.clone()).or_insert(0);
                *count += 1;
                let mut peak = self.peak.lock().expect("peak map");
                let high = peak.entry(room.clone()).or_insert(0);
                *high = (*high).max(*count);
            }
            let total = self.live_total.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak_total.fetch_max(total, Ordering::SeqCst);

            tokio::time::sleep(self.delay).await;

            self.calls.fetch_add(1, Ordering::SeqCst);
            {
                let mut live = self.live.lock().expect("live map");
                if let Some(count) = live.get_mut(&room) {
                    *count = count.saturating_sub(1);
                }
            }
            self.live_total.fetch_sub(1, Ordering::SeqCst);
            Ok(Response::json(200, r#"{"event_id":"$ok"}"#))
        })
    }
}

#[test]
fn the_client_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MatrixClient>();
    assert_send_sync::<ClientConfig>();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thirty_two_tasks_share_one_client_and_never_collide_on_a_transaction_id() {
    let transport = Arc::new(StubTransport::new());
    for _ in 0..TASKS * 4 {
        transport.push_json(200, r#"{"event_id":"$ok"}"#);
    }
    let client = Arc::new(client(transport.clone()));

    let mut handles = Vec::with_capacity(TASKS);
    for task in 0..TASKS {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            let mut outcomes = Vec::new();
            for message in 0..4 {
                let room = format!("!room-{task}-{message}:m.example");
                outcomes.push(
                    client
                        .send_text(&room, "Hallo")
                        .await
                        .expect("every send lands"),
                );
            }
            outcomes
        }));
    }

    let mut txn_ids = HashSet::new();
    let mut event_count = 0;
    for handle in handles {
        for outcome in handle.await.expect("no task panicked") {
            assert_eq!(outcome.event_id, "$ok");
            assert_eq!(outcome.attempts, 1);
            assert!(txn_ids.insert(outcome.txn_id.clone()), "a txn id repeated");
            event_count += 1;
        }
    }
    assert_eq!(event_count, TASKS * 4);
    assert_eq!(transport.call_count(), TASKS * 4);
    // Every request carried a distinct transaction id in its URL.
    let urls: HashSet<String> = transport
        .requests()
        .into_iter()
        .map(|request| request.url)
        .collect();
    assert_eq!(urls.len(), TASKS * 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_room_never_has_two_sends_in_flight() {
    let transport = SlowTransport::new(Duration::from_millis(10));
    let client = Arc::new(client(Arc::clone(&transport) as Arc<dyn Transport>));

    let mut handles = Vec::with_capacity(TASKS);
    for _ in 0..TASKS {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            client.send_text("!busy:m.example", "Hallo").await
        }));
    }
    for handle in handles {
        handle.await.expect("no task panicked").expect("sent");
    }

    assert_eq!(
        transport.calls(),
        u32::try_from(TASKS).expect("TASKS fits in u32")
    );
    assert_eq!(
        transport.peak_for("!busy:m.example"),
        1,
        "a second send entered the same room while the first was in flight"
    );
    // Serialisation costs throughput, not correctness: all 32 arrived.
    assert_eq!(transport.peak_total(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_rooms_are_serialised_independently() {
    let transport = SlowTransport::new(Duration::from_millis(15));
    let client = Arc::new(client(Arc::clone(&transport) as Arc<dyn Transport>));

    let mut handles = Vec::with_capacity(TASKS);
    for task in 0..TASKS {
        let client = Arc::clone(&client);
        let room = if task % 2 == 0 {
            "!a:m.example"
        } else {
            "!b:m.example"
        };
        handles.push(tokio::spawn(async move {
            client.send_text(room, "Hallo").await
        }));
    }
    for handle in handles {
        handle.await.expect("no task panicked").expect("sent");
    }

    assert_eq!(transport.peak_for("!a:m.example"), 1);
    assert_eq!(transport.peak_for("!b:m.example"), 1);
    assert!(
        transport.peak_total() >= 2,
        "two rooms must be able to send at the same time, peak was {}",
        transport.peak_total()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordering_off_admits_concurrent_sends_into_one_room() {
    let transport = SlowTransport::new(Duration::from_millis(20));
    let client = Arc::new(
        MatrixClient::new(
            Arc::clone(&transport) as Arc<dyn Transport>,
            ClientConfig::new("https://m.example")
                .expect("valid")
                .with_per_room_ordering(false),
            "syt",
        )
        .expect("valid"),
    );

    let mut handles = Vec::with_capacity(TASKS);
    for _ in 0..TASKS {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            client.send_text("!busy:m.example", "Hallo").await
        }));
    }
    for handle in handles {
        handle.await.expect("no task panicked").expect("sent");
    }

    assert!(
        transport.peak_for("!busy:m.example") > 1,
        "with ordering off the room should not be a bottleneck"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retries_do_not_hold_a_room_gate_across_tasks() {
    let transport = Arc::new(StubTransport::new());
    // Every send is rate-limited once, so every send sleeps and retries.
    for _ in 0..TASKS {
        transport
            .push_json(429, r#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":1}"#)
            .push_json(200, r#"{"event_id":"$ok"}"#);
    }
    let client = Arc::new(client(transport.clone()));

    let mut handles = Vec::with_capacity(TASKS);
    for _ in 0..TASKS {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            client.send_text("!limited:m.example", "Hallo").await
        }));
    }
    let mut attempts = 0;
    for handle in handles {
        let outcome = handle.await.expect("no task panicked").expect("sent");
        attempts += outcome.attempts;
    }
    assert_eq!(
        attempts,
        u32::try_from(TASKS).expect("TASKS fits in u32") * 2
    );
    assert_eq!(transport.call_count(), TASKS * 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_leg_is_safe_to_call_from_many_tasks_at_once() {
    let transport = Arc::new(StubTransport::new());
    // One answer that satisfies every leg at once: the tasks race, so which
    // response a given call gets must not matter. `whoami` reads `user_id`,
    // an upload reads `content_uri`, and typing/receipts ignore the body.
    for _ in 0..TASKS {
        transport.push_json(
            200,
            r#"{"user_id":"@bot:m.example","content_uri":"mxc://m.example/a"}"#,
        );
    }
    let client = Arc::new(client(transport.clone()));

    let mut handles = Vec::with_capacity(TASKS);
    for task in 0..TASKS {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            match task % 4 {
                0 => client.whoami().await.map(|user| user.len()),
                1 => client.send_typing("!r:m.example", 1_000).await.map(|()| 0),
                2 => client
                    .send_read_receipt("!r:m.example", "$abc")
                    .await
                    .map(|()| 0),
                _ => client
                    .upload_media(&messenger_webhook::matrix_out::MediaUpload::new(
                        b"x",
                        "text/plain",
                    ))
                    .await
                    .map(|uri| uri.as_str().len()),
            }
        }));
    }
    for handle in handles {
        let result = handle.await.expect("no task panicked");
        assert!(result.is_ok(), "{result:?}");
    }
    assert_eq!(transport.call_count(), TASKS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_room_does_not_starve_the_others() {
    let transport = Arc::new(StubTransport::new());
    for _ in 0..TASKS / 2 {
        // Half the sends are refused outright; the other half succeed.
        transport.push_json(403, r#"{"errcode":"M_FORBIDDEN","error":"not joined"}"#);
        transport.push_json(200, r#"{"event_id":"$ok"}"#);
    }
    let client = Arc::new(client(transport.clone()));

    let mut handles = Vec::with_capacity(TASKS);
    for task in 0..TASKS {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            let room = format!("!room-{task}:m.example");
            client.send_text(&room, "Hallo").await
        }));
    }
    let mut ok = 0;
    let mut refused = 0;
    for handle in handles {
        match handle.await.expect("no task panicked") {
            Ok(_) => ok += 1,
            Err(error) => {
                assert!(error.to_string().contains("403"), "{error}");
                refused += 1;
            }
        }
    }
    assert_eq!(ok + refused, TASKS);
    assert_eq!(transport.call_count(), TASKS);
}
