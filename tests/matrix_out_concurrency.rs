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
use messenger_webhook::matrix_out::{ClientConfig, MatrixClient, Sleeper, StubTransport};

/// Tasks per test. Well above the core count, so any shared-state bug shows.
const TASKS: usize = 32;

fn client(transport: Arc<dyn Transport>) -> MatrixClient {
    MatrixClient::new(
        transport,
        ClientConfig::new("https://m.example")
            .expect("valid")
            .with_user_id("@bot:m.example"),
        "syt_token_123456",
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

/// A transport that rate-limits the first call for each transaction id, then
/// accepts that same transaction.
#[derive(Debug, Default)]
struct RetryOnceTransport {
    seen: Mutex<HashMap<String, u32>>,
    calls: AtomicU32,
}

impl RetryOnceTransport {
    fn call_count(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Transport for RetryOnceTransport {
    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Response, TransportError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let txn = request
                .url()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            let mut seen = self.seen.lock().expect("seen map");
            let attempts = seen.entry(txn).or_insert(0);
            *attempts += 1;
            if *attempts == 1 {
                Ok(Response::json(
                    429,
                    r#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":1}"#,
                ))
            } else {
                Ok(Response::json(200, r#"{"event_id":"$ok"}"#))
            }
        })
    }
}

/// A sleeper that blocks the first retry until the test releases it.
#[derive(Debug)]
struct BlockingSleeper {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl BlockingSleeper {
    fn new() -> Self {
        Self {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        }
    }

    async fn wait_started(&self) {
        self.started.notified().await;
    }

    fn release(&self) {
        self.release.notify_one();
    }
}

impl Sleeper for BlockingSleeper {
    fn sleep(&self, _delay: Duration) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
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
            "syt_token_123456",
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
    let transport = Arc::new(RetryOnceTransport::default());
    // Every send is rate-limited once, so every send sleeps and retries.
    let client = Arc::new(client(Arc::clone(&transport) as Arc<dyn Transport>));

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
    assert_eq!(
        transport.call_count(),
        u32::try_from(TASKS).expect("TASKS fits in u32") * 2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retry_sleep_does_not_hold_the_room_gate() {
    let transport = Arc::new(StubTransport::new());
    transport
        .push_json(
            429,
            r#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":1000}"#,
        )
        .push_json(200, r#"{"event_id":"$first"}"#)
        .push_json(200, r#"{"event_id":"$second"}"#);
    let sleeper = Arc::new(BlockingSleeper::new());
    let client = Arc::new(
        MatrixClient::new(
            transport.clone(),
            ClientConfig::new("https://m.example")
                .expect("valid")
                .with_sleeper(sleeper.clone()),
            "syt_token_123456",
        )
        .expect("valid client"),
    );

    let first_client = Arc::clone(&client);
    let first =
        tokio::spawn(async move { first_client.send_text("!busy:m.example", "first").await });
    sleeper.wait_started().await;

    let second = tokio::time::timeout(
        Duration::from_millis(200),
        client.send_text("!busy:m.example", "second"),
    )
    .await;

    sleeper.release();
    let first = first
        .await
        .expect("no task panicked")
        .expect("the first send eventually succeeds");
    let second = second
        .expect("the second send must not wait for the first retry sleep")
        .expect("second send");

    assert_eq!(first.attempts, 2);
    assert_eq!(second.attempts, 1);
    assert_eq!(
        [first.event_id.as_str(), second.event_id.as_str()]
            .into_iter()
            .collect::<HashSet<_>>(),
        HashSet::from(["$first", "$second"])
    );
    assert_eq!(transport.call_count(), 3);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_routed_script_answers_the_intended_room_across_tasks() {
    // The exact concurrency hazard the audit named: a 429 scripted for one send
    // must not land on whichever task reaches the stub first. Room `b` is
    // rate-limited once then succeeds; every other room succeeds immediately
    // from the FIFO fallback. All of them race on one client and one stub.
    let transport = Arc::new(StubTransport::new());
    transport.push_json_for(
        messenger_webhook::matrix_out::transport::Method::Put,
        "room-b",
        429,
        r#"{"errcode":"M_LIMIT_EXCEEDED","retry_after_ms":1}"#,
    );
    transport.push_json_for(
        messenger_webhook::matrix_out::transport::Method::Put,
        "room-b",
        200,
        r#"{"event_id":"$b"}"#,
    );
    for _ in 0..TASKS {
        transport.push_json(200, r#"{"event_id":"$ok"}"#);
    }
    let client = Arc::new(client(transport.clone()));

    let mut handles = Vec::with_capacity(TASKS);
    // One task for the rate-limited room, then the rest for unrelated rooms.
    for task in 0..=TASKS {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            let room = if task == TASKS {
                "!room-b:m.example".to_owned()
            } else {
                format!("!room-{task}:m.example")
            };
            (task == TASKS, client.send_text(&room, "Hallo").await)
        }));
    }
    let mut limited_attempts = None;
    for handle in handles {
        let (is_limited_room, result) = handle.await.expect("no task panicked");
        let outcome = result.expect("sent");
        if is_limited_room {
            limited_attempts = Some(outcome.attempts);
        } else {
            assert_eq!(outcome.attempts, 1, "an unrelated room absorbed the 429");
        }
    }
    assert_eq!(limited_attempts, Some(2), "room b must hit exactly one 429");

    // The recorded attempt index makes the retried transaction assertable: the
    // room-b URL appears twice, at two distinct positions in the record.
    let attempts: Vec<usize> = transport
        .requests()
        .iter()
        .filter(|request| request.url.contains("room-b"))
        .map(|request| request.attempt)
        .collect();
    assert_eq!(attempts.len(), 2);
    assert_ne!(attempts[0], attempts[1]);
}
