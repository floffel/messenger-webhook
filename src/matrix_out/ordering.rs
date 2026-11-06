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
//! The gate map is bounded ([`GATE_CAPACITY`] rooms). Beyond it, gates for rooms
//! with no send in flight are dropped; if that frees nothing, the send proceeds
//! *without* a gate rather than growing the map without limit — ordering
//! degrades, memory does not.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::matrix_out::{types, OutboundError};

/// How many room gates the client keeps before pruning idle ones.
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

/// One async mutex per room, created on demand.
#[derive(Debug)]
pub(crate) struct RoomGates {
    gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    capacity: usize,
}

impl RoomGates {
    /// An empty gate map with `capacity` slots.
    pub(crate) fn new(capacity: usize) -> Result<Self, OutboundError> {
        if capacity == 0 {
            return Err(OutboundError::InvalidConfig {
                reason: "per-room gate capacity is 0, which refuses every gate".to_owned(),
            });
        }
        Ok(Self {
            gates: Mutex::new(HashMap::new()),
            capacity,
        })
    }

    /// Number of gates currently held, for tests and `Debug`.
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }

    /// Waits until no other send is in flight for `room_id` and returns the
    /// guard that keeps it that way.
    ///
    /// `None` means the map is full of gates that are all in use, so the caller
    /// proceeds unordered. The lock on the map is released before the room gate
    /// is awaited — a guard is never held across an `.await`.
    pub(crate) async fn acquire(&self, room_id: RoomKey<'_>) -> Option<OwnedMutexGuard<()>> {
        let gate = {
            let mut gates = self.lock();
            if let Some(existing) = gates.get(room_id.as_str()) {
                Arc::clone(existing)
            } else {
                if gates.len() >= self.capacity {
                    // Drop the gates nobody is waiting on, then re-check.
                    gates.retain(|_, gate| Arc::strong_count(gate) > 1);
                    if gates.len() >= self.capacity {
                        return None;
                    }
                }
                let created = Arc::new(AsyncMutex::new(()));
                gates.insert(room_id.as_str().to_owned(), Arc::clone(&created));
                created
            }
        };
        // `lock_owned` on an `Arc<AsyncMutex>` keeps the gate alive for as long
        // as the guard exists, even if the map drops its reference meanwhile.
        Some(gate.lock_owned().await)
    }

    /// A poisoned map is recovered, never a panic: the map holds no invariants
    /// that a panic mid-insert could have broken.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<AsyncMutex<()>>>> {
        self.gates.lock().unwrap_or_else(PoisonError::into_inner)
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
            assert_eq!(gates.len(), 1);
            drop(first);
            let second = gates.acquire(key("!a:x")).await;
            assert!(second.is_some());
            // Same room, same gate: no second entry.
            assert_eq!(gates.len(), 1);
            let other = gates.acquire(key("!b:x")).await;
            assert!(other.is_some());
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
    fn a_full_map_of_in_flight_gates_degrades_instead_of_growing() {
        runtime().block_on(async {
            let gates = gates(2);
            let first = gates.acquire(key("!a:x")).await;
            let second = gates.acquire(key("!b:x")).await;
            assert!(first.is_some() && second.is_some());
            assert_eq!(gates.len(), 2);

            // Both slots are in use: a third room gets no gate at all.
            assert!(gates.acquire(key("!c:x")).await.is_none());
            assert_eq!(gates.len(), 2);

            // Releasing one slot lets the map reclaim it and admit the new room.
            drop(first);
            let third = gates.acquire(key("!c:x")).await;
            assert!(third.is_some(), "an idle gate must be pruned");
            assert_eq!(gates.len(), 2);
        });
    }

    #[test]
    fn zero_capacity_is_rejected() {
        let error = RoomGates::new(0).expect_err("capacity 0 is unusable");

        assert!(matches!(error, OutboundError::InvalidConfig { .. }));
        assert!(error.to_string().contains("capacity is 0"), "{error}");
    }

    #[test]
    fn capacity_one_holds_one_gate_and_degrades_on_the_next_room() {
        runtime().block_on(async {
            let gates = gates(1);
            let first = gates.acquire(key("!a:x")).await;
            assert!(first.is_some());
            assert_eq!(gates.len(), 1);

            assert!(gates.acquire(key("!b:x")).await.is_none());
            assert_eq!(gates.len(), 1);

            drop(first);
            let second = gates.acquire(key("!b:x")).await;
            assert!(second.is_some(), "the idle gate must be pruned");
            assert_eq!(gates.len(), 1);
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
            assert_eq!(gates.len(), 3);

            assert!(gates.acquire(key("!d:x")).await.is_none());
            assert_eq!(gates.len(), 3);

            drop(first);
            let fourth = gates.acquire(key("!d:x")).await;
            assert!(fourth.is_some());
            assert_eq!(gates.len(), 3);
        });
    }

    #[test]
    fn a_live_gate_survives_pruning_under_pressure() {
        runtime().block_on(async {
            let gates = gates(1);
            let held = gates.acquire(key("!live:x")).await.expect("a gate");

            assert!(gates.acquire(key("!new:x")).await.is_none());
            assert!(gates.lock().contains_key("!live:x"));
            assert_eq!(gates.len(), 1);

            drop(held);
        });
    }

    #[test]
    fn a_queued_waiter_survives_pruning_under_pressure() {
        runtime().block_on(async {
            let gates = gates(1);
            let held = gates.acquire(key("!queued:x")).await.expect("a gate");
            let mut waiter = Box::pin(gates.acquire(key("!queued:x")));

            assert!(poll_once(waiter.as_mut()).is_pending());
            assert!(gates.acquire(key("!new:x")).await.is_none());
            assert!(gates.lock().contains_key("!queued:x"));
            assert_eq!(gates.len(), 1);

            drop(held);
            assert!(poll_once(waiter.as_mut()).is_ready());
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

    #[test]
    fn a_poisoned_map_is_recovered_rather_than_panicking() {
        let gates = Arc::new(gates(GATE_CAPACITY));
        let poisoner = Arc::clone(&gates);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("poison the gate map on purpose");
        })
        .join();
        assert!(gates.lock().is_empty());
        let acquired = runtime().block_on(gates.acquire(key("!a:x")));
        assert!(acquired.is_some());
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
                    prop_assert!(gates.acquire(key(&room)).await.is_none());
                    prop_assert!(gates.len() <= capacity);
                }

                for index in 0..capacity {
                    let room = format!("!held-{index}:x");
                    prop_assert!(gates.lock().contains_key(&room));
                }
                prop_assert!(gates.lock().contains_key(&queued_room));
                prop_assert_eq!(gates.len(), capacity);
                Ok(())
            });
            prop_assert!(result.is_ok(), "{result:?}");
        }
    }
}
