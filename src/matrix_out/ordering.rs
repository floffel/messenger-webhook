//! Per-room send ordering.
//!
//! Matrix does not *require* a client to serialise its sends: the homeserver
//! stamps every event with an arrival time and the room is a DAG. But two sends
//! fired concurrently into one room can be ordered by whichever finished first,
//! and for a bot writing a sequence ("your contract is attached" → the PDF) that
//! is a visible defect. The client therefore takes a per-room gate — one
//! in-flight request per room, any number across rooms — and releases it when
//! the operation completes, retries included.
//!
//! The gate map is bounded ([`GATE_CAPACITY`] rooms). Beyond it, gates for rooms
//! with no send in flight are dropped; if that frees nothing, the send proceeds
//! *without* a gate rather than growing the map without limit — ordering
//! degrades, memory does not.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// How many room gates the client keeps before pruning idle ones.
///
/// ```
/// use messenger_webhook::matrix_out::GATE_CAPACITY;
///
/// assert_eq!(GATE_CAPACITY, 4_096);
/// ```
pub const GATE_CAPACITY: usize = 4_096;

/// One async mutex per room, created on demand.
#[derive(Debug)]
pub(crate) struct RoomGates {
    gates: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    capacity: usize,
}

impl RoomGates {
    /// An empty gate map with `capacity` slots.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            gates: Mutex::new(HashMap::new()),
            capacity,
        }
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
    pub(crate) async fn acquire(&self, room_id: &str) -> Option<OwnedMutexGuard<()>> {
        let gate = {
            let mut gates = self.lock();
            if let Some(existing) = gates.get(room_id) {
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
                gates.insert(room_id.to_owned(), Arc::clone(&created));
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
    use std::time::Duration;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime")
    }

    #[test]
    fn gates_are_created_per_room_and_reused() {
        runtime().block_on(async {
            let gates = RoomGates::new(GATE_CAPACITY);
            assert_eq!(gates.len(), 0);
            let first = gates.acquire("!a:x").await;
            assert!(first.is_some());
            assert_eq!(gates.len(), 1);
            drop(first);
            let second = gates.acquire("!a:x").await;
            assert!(second.is_some());
            // Same room, same gate: no second entry.
            assert_eq!(gates.len(), 1);
            let other = gates.acquire("!b:x").await;
            assert!(other.is_some());
            assert_eq!(gates.len(), 2);
        });
    }

    #[test]
    fn one_room_serialises_and_two_rooms_do_not() {
        runtime().block_on(async {
            let gates = Arc::new(RoomGates::new(GATE_CAPACITY));
            // A gate held for room A must block a second acquire of room A but
            // not of room B.
            let held = gates.acquire("!a:x").await.expect("a free gate");
            let blocked = {
                let gates = Arc::clone(&gates);
                tokio::spawn(async move { gates.acquire("!a:x").await })
            };
            let free = gates.acquire("!b:x").await;
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
            let gates = RoomGates::new(2);
            let first = gates.acquire("!a:x").await;
            let second = gates.acquire("!b:x").await;
            assert!(first.is_some() && second.is_some());
            assert_eq!(gates.len(), 2);

            // Both slots are in use: a third room gets no gate at all.
            assert!(gates.acquire("!c:x").await.is_none());
            assert_eq!(gates.len(), 2);

            // Releasing one slot lets the map reclaim it and admit the new room.
            drop(first);
            let third = gates.acquire("!c:x").await;
            assert!(third.is_some(), "an idle gate must be pruned");
            assert_eq!(gates.len(), 2);
        });
    }

    #[test]
    fn a_poisoned_map_is_recovered_rather_than_panicking() {
        let gates = Arc::new(RoomGates::new(GATE_CAPACITY));
        let poisoner = Arc::clone(&gates);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("poison the gate map on purpose");
        })
        .join();
        assert!(gates.lock().is_empty());
        let acquired = runtime().block_on(gates.acquire("!a:x"));
        assert!(acquired.is_some());
    }
}
