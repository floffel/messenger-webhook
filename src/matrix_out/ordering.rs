//! Per-room send ordering.
//!
//! Matrix does not *require* a client to serialise its sends: the homeserver
//! stamps every event with an arrival time and the room is a DAG. But two sends
//! fired concurrently into one room can be ordered by whichever finished first,
//! and for a bot writing a sequence ("your contract is attached" → the PDF) that
//! is a visible defect. The client therefore takes a per-room gate — one
//! in-flight request per room, any number across rooms — and releases it between
//! attempts, so one rate-limited send does not block the room through its retry
//! sleeps.
//!
//! The gate cache is bounded ([`GATE_CAPACITY`] rooms). Moka's eviction policy
//! keeps the cache near that bound, so one room's sends never cause a full-map
//! scan or block a Tokio worker while another room is acquiring a gate.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use moka::future::Cache;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::matrix_out::{types, OutboundError};

/// How many room gates the client keeps before moka evicts old entries.
///
/// ```
/// use messenger_webhook::matrix_out::GATE_CAPACITY;
///
/// assert_eq!(GATE_CAPACITY, 4_096);
/// ```
pub const GATE_CAPACITY: usize = 4_096;

/// A room address that has passed the id checks the send endpoints require.
///
/// The gate map never sees the caller's raw string: constructing this key is
/// the only way to acquire a gate, so an alias or an oversized id cannot create
/// a second, unbounded entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RoomKey<'a>(&'a str);

impl<'a> RoomKey<'a> {
    /// Rejects anything the Matrix send endpoints cannot address.
    pub(crate) fn new(room_id: &'a str) -> Result<Self, OutboundError> {
        types::validate_room_id(room_id)?;
        Ok(Self(room_id))
    }

    /// The validated room id, still borrowed from the caller.
    fn as_str(&self) -> &'a str {
        self.0
    }
}

/// A room gate and the number of callers waiting on or holding it.
#[derive(Debug)]
struct Gate {
    mutex: Arc<AsyncMutex<()>>,
    active: AtomicUsize,
}

impl Gate {
    fn new() -> Self {
        Self {
            mutex: Arc::new(AsyncMutex::new(())),
            active: AtomicUsize::new(0),
        }
    }
}

/// Tracks one caller's interest in a gate until it has a guard, or is cancelled.
#[derive(Debug)]
struct GateLease {
    gate: Arc<Gate>,
}

impl GateLease {
    fn new(gate: Arc<Gate>) -> Self {
        gate.active.fetch_add(1, Ordering::AcqRel);
        Self { gate }
    }
}

impl Drop for GateLease {
    fn drop(&mut self) {
        self.gate.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The per-room ordering guard returned by [`RoomGates::acquire`].
#[derive(Debug)]
pub(crate) struct GateGuard {
    _guard: OwnedMutexGuard<()>,
    _lease: GateLease,
}

/// One async mutex per room, created on demand.
#[derive(Debug)]
pub(crate) struct RoomGates {
    gates: Cache<String, Arc<Gate>>,
}

impl RoomGates {
    /// An empty gate cache with `capacity` slots.
    pub(crate) fn new(capacity: usize) -> Result<Self, OutboundError> {
        if capacity == 0 {
            return Err(OutboundError::InvalidConfig {
                reason: "per-room gate capacity is 0, which refuses every gate".to_owned(),
            });
        }
        Ok(Self {
            gates: Cache::new(capacity as u64),
        })
    }

    /// Number of gates currently cached, for tests and `Debug`.
    pub(crate) fn len(&self) -> usize {
        usize::try_from(self.gates.entry_count()).unwrap_or(usize::MAX)
    }

    /// Waits until no other send is in flight for `room_id` and returns the
    /// guard that keeps it that way.
    ///
    /// Moka's cache lock is internal and policy-driven; no map lock is held
    /// across the room gate's `.await`.
    pub(crate) async fn acquire(&self, room_id: RoomKey<'_>) -> Option<GateGuard> {
        let gate = self
            .gates
            .get_with(room_id.as_str().to_owned(), async { Arc::new(Gate::new()) })
            .await;
        let lease = GateLease::new(Arc::clone(&gate));
        // If this acquire is cancelled while waiting, `lease` drops and the
        // gate may be evicted. While it is held, the caller's lease and tokio
        // guard keep the room serialised.
        let guard = Arc::clone(&gate.mutex).lock_owned().await;
        Some(GateGuard {
            _guard: guard,
            _lease: lease,
        })
    }

    #[cfg(test)]
    async fn run_pending_tasks(&self) {
        self.gates.run_pending_tasks().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matrix_out::MAX_ID_BYTES;
    use proptest::prelude::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime")
    }

    fn key(room_id: &str) -> RoomKey<'_> {
        RoomKey::new(room_id).expect("a valid room id")
    }

    fn gates(capacity: usize) -> RoomGates {
        RoomGates::new(capacity).expect("a non-zero capacity")
    }

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        let mut context = Context::from_waker(Waker::noop());
        future.poll(&mut context)
    }

    #[test]
    fn gates_are_created_per_room_and_reused() {
        runtime().block_on(async {
            let gates = gates(GATE_CAPACITY);
            assert_eq!(gates.len(), 0);
            let first = gates.acquire(key("!a:x")).await;
            assert!(first.is_some());
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 1);
            drop(first);
            let second = gates.acquire(key("!a:x")).await;
            assert!(second.is_some());
            gates.run_pending_tasks().await;
            // Same room, same gate: no second entry.
            assert_eq!(gates.len(), 1);
            let other = gates.acquire(key("!b:x")).await;
            assert!(other.is_some());
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 2);
        });
    }

    #[test]
    fn one_room_serialises_and_two_rooms_do_not() {
        runtime().block_on(async {
            let gates = Arc::new(gates(GATE_CAPACITY));
            // A gate held for room A must block a second acquire of room A but
            // not of room B.
            let held = gates.acquire(key("!a:x")).await.expect("a free gate");
            let blocked = {
                let gates = Arc::clone(&gates);
                tokio::spawn(async move { gates.acquire(key("!a:x")).await })
            };
            let free = gates.acquire(key("!b:x")).await;
            assert!(free.is_some(), "a different room must not block");
            tokio::time::sleep(Duration::from_millis(10)).await;
            assert!(!blocked.is_finished(), "the same room must block");
            drop(held);
            assert!(blocked.await.expect("task").is_some());
        });
    }

    #[test]
    fn a_full_cache_admits_new_rooms_by_evicting_old_ones() {
        runtime().block_on(async {
            let gates = gates(2);
            let first = gates.acquire(key("!a:x")).await;
            let second = gates.acquire(key("!b:x")).await;
            assert!(first.is_some() && second.is_some());
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 2);

            let third = gates.acquire(key("!c:x")).await;
            assert!(third.is_some());
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 2);

            drop((first, second, third));
        });
    }

    #[test]
    fn zero_capacity_is_rejected() {
        let error = RoomGates::new(0).expect_err("capacity 0 is unusable");

        assert!(matches!(error, OutboundError::InvalidConfig { .. }));
        assert!(error.to_string().contains("capacity is 0"), "{error}");
    }

    #[test]
    fn capacity_one_keeps_the_cache_bounded() {
        runtime().block_on(async {
            let gates = gates(1);
            let first = gates.acquire(key("!a:x")).await;
            assert!(first.is_some());
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 1);

            let second = gates.acquire(key("!b:x")).await;
            assert!(second.is_some());
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 1);

            drop((first, second));
        });
    }

    #[test]
    fn capacity_is_an_exact_boundary_not_an_exclusive_one() {
        runtime().block_on(async {
            let gates = gates(3);
            let first = gates.acquire(key("!a:x")).await;
            let second = gates.acquire(key("!b:x")).await;
            let third = gates.acquire(key("!c:x")).await;
            assert!(first.is_some() && second.is_some() && third.is_some());
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 3);

            let fourth = gates.acquire(key("!d:x")).await;
            assert!(fourth.is_some());
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 3);

            drop((first, second, third, fourth));
        });
    }

    #[test]
    fn eviction_does_not_lose_tokio_serialization_for_a_live_room() {
        runtime().block_on(async {
            let gates = Arc::new(gates(1));
            let held = gates.acquire(key("!live:x")).await.expect("a gate");

            let blocked = {
                let gates = Arc::clone(&gates);
                tokio::spawn(async move { gates.acquire(key("!live:x")).await })
            };
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 1);
            tokio::time::sleep(Duration::from_millis(10)).await;
            assert!(!blocked.is_finished(), "the held room must still block");

            drop(held);
            assert!(blocked.await.expect("task").is_some());
        });
    }

    #[test]
    fn a_cache_miss_for_the_same_room_blocks_until_the_previous_guard_drops() {
        runtime().block_on(async {
            let gates = Arc::new(gates(1));
            let held = gates.acquire(key("!queued:x")).await.expect("a gate");
            let mut waiter = Box::pin(gates.acquire(key("!queued:x")));

            assert!(poll_once(waiter.as_mut()).is_pending());
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 1);

            drop(held);
            assert!(poll_once(waiter.as_mut()).is_ready());
        });
    }

    #[test]
    fn a_cancelled_waiter_does_not_block_the_room() {
        runtime().block_on(async {
            let gates = gates(1);
            let held = gates.acquire(key("!queued:x")).await.expect("a gate");
            let mut waiter = Box::pin(gates.acquire(key("!queued:x")));

            assert!(poll_once(waiter.as_mut()).is_pending());
            drop(waiter);

            drop(held);
            let new = gates.acquire(key("!queued:x")).await;
            assert!(new.is_some(), "the cancelled waiter must not leak the room");
            gates.run_pending_tasks().await;
            assert_eq!(gates.len(), 1);
        });
    }

    #[test]
    fn an_alias_is_not_a_room_key() {
        let error = RoomKey::new("#alias:x").expect_err("an alias");

        assert!(matches!(error, OutboundError::InvalidRequest { .. }));
        assert!(error.to_string().contains("resolve a `#alias`"), "{error}");
    }

    #[test]
    fn room_keys_are_bounded_before_they_can_be_stored() {
        let prefix = "!r:";
        let exact = format!("{prefix}{}", "a".repeat(MAX_ID_BYTES - prefix.len()));
        assert!(RoomKey::new(&exact).is_ok());

        let over = format!("{prefix}{}", "a".repeat(MAX_ID_BYTES - prefix.len() + 1));
        let error = RoomKey::new(&over).expect_err("one byte too long");

        assert!(matches!(error, OutboundError::InvalidRequest { .. }));
        assert!(error.to_string().contains("256"), "{error}");
    }

    proptest! {
        #[test]
        fn gate_count_never_exceeds_capacity(capacity in 1usize..8, rooms in 1usize..24) {
            let result = runtime().block_on(async {
                let gates = gates(capacity);
                let mut held = Vec::new();
                for index in 0..rooms {
                    let room = format!("!{index}:x");
                    if let Some(gate) = gates.acquire(key(&room)).await {
                        held.push(gate);
                    }
                    gates.run_pending_tasks().await;
                    prop_assert!(gates.len() <= capacity);
                }
                Ok(())
            });
            prop_assert!(result.is_ok(), "{result:?}");
        }

        #[test]
        fn pruning_keeps_every_gate_with_a_live_or_queued_waiter(
            capacity in 1usize..6,
            extra in 1usize..12,
        ) {
            let result = runtime().block_on(async {
                let gates = gates(capacity);
                let mut held = Vec::new();
                for index in 0..capacity {
                    let room = format!("!held-{index}:x");
                    held.push(
                        gates
                            .acquire(key(&room))
                            .await
                            .expect("capacity must admit every slot"),
                    );
                }

                let queued_room = "!held-0:x".to_owned();
                let mut waiter = Box::pin(gates.acquire(key(&queued_room)));
                prop_assert!(poll_once(waiter.as_mut()).is_pending());

                for index in 0..extra {
                    let room = format!("!extra-{index}:x");
                    prop_assert!(gates.acquire(key(&room)).await.is_some());
                    gates.run_pending_tasks().await;
                    prop_assert!(gates.len() <= capacity);
                }

                gates.run_pending_tasks().await;
                prop_assert!(gates.len() <= capacity);
                Ok(())
            });
            prop_assert!(result.is_ok(), "{result:?}");
        }
    }
}
