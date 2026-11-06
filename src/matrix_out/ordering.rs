//! Per-room send ordering.
//!
//! Matrix does not *require* a client to serialise its sends: the homeserver
//! stamps every event with an arrival time and the room is a DAG. But two sends
//! fired concurrently into one room can be ordered by whichever finished first,
//! and for a bot writing a sequence ("your contract is attached" -> the PDF) that
//! is a visible defect. The client therefore takes a per-room gate -- one
//! in-flight request per room, any number across rooms -- and releases it between
//! attempts, so one rate-limited send does not block the room through its retry
//! sleeps.
//!
//! The gate registry is bounded by count ([`GATE_CAPACITY`] active rooms). A
//! gate entry is removed only when its last holder or waiter drops, so a room
//! cannot be silently recreated while an earlier send for that room is still
//! queued. When every slot is in use, [`OrderingSaturation`] decides whether a
//! new room waits for capacity (the default), receives
//! [`OutboundError::OrderingLost`], or explicitly opts into unordered delivery.
//!
//! Per-room waits are bounded too. At most [`GATE_WAITERS_PER_ROOM`] callers may
//! queue behind the in-flight holder, and a waiter that cannot acquire the room
//! within [`GATE_WAIT_TIMEOUT`] gets a typed [`OutboundError::GateTimeout`]
//! instead of awaiting forever.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

use crate::matrix_out::{types, OutboundError};

/// How many room gates the client keeps before saturation policy applies.
///
/// ```
/// use messenger_webhook::matrix_out::GATE_CAPACITY;
///
/// assert_eq!(GATE_CAPACITY, 4_096);
/// ```
pub const GATE_CAPACITY: usize = 4_096;

/// How many callers may wait behind one in-flight send in a room.
///
/// This is a count of waiters per room, not a byte bound. The holder itself is
/// admitted separately, so a room may have this many queued callers plus the
/// active sender before the next caller gets [`OutboundError::GateOverflow`].
///
/// ```
/// use messenger_webhook::matrix_out::GATE_WAITERS_PER_ROOM;
///
/// assert_eq!(GATE_WAITERS_PER_ROOM, 64);
/// ```
pub const GATE_WAITERS_PER_ROOM: usize = 64;

/// How long one caller may wait for a room's in-flight send to finish.
///
/// ```
/// use std::time::Duration;
/// use messenger_webhook::matrix_out::GATE_WAIT_TIMEOUT;
///
/// assert_eq!(GATE_WAIT_TIMEOUT, Duration::from_secs(30));
/// ```
pub const GATE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// What to do when all [`GATE_CAPACITY`] gates are held by active rooms.
///
/// The default is [`OrderingSaturation::Backpressure`]: a send to a new room
/// waits until another room finishes instead of silently losing its ordering
/// guarantee. Deployments that prefer an explicit failure can use
/// [`OrderingSaturation::Error`], and callers that intentionally accept
/// unordered delivery under pressure can choose [`OrderingSaturation::Unordered`].
///
/// ```
/// use messenger_webhook::matrix_out::OrderingSaturation;
///
/// assert_eq!(OrderingSaturation::default(), OrderingSaturation::Backpressure);
/// assert_eq!(OrderingSaturation::Backpressure.as_str(), "backpressure");
/// assert_eq!(OrderingSaturation::Error.as_str(), "error");
/// assert_eq!(OrderingSaturation::Unordered.as_str(), "unordered");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum OrderingSaturation {
    /// Wait until another room's gate is released, then acquire the new room's
    /// gate. This preserves the ordering contract under load.
    #[default]
    Backpressure,
    /// Fail the send with [`OutboundError::OrderingLost`] instead of waiting.
    Error,
    /// Proceed without a gate for the new room. This is an explicit opt-in to
    /// degraded ordering, not a silent fallback.
    Unordered,
}

impl OrderingSaturation {
    /// Stable lower-case name, used in logs and `Debug` output.
    ///
    /// ```
    /// use messenger_webhook::matrix_out::OrderingSaturation;
    ///
    /// assert_eq!(OrderingSaturation::Backpressure.as_str(), "backpressure");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Backpressure => "backpressure",
            Self::Error => "error",
            Self::Unordered => "unordered",
        }
    }
}

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
    waiters: Arc<Semaphore>,
    active: AtomicUsize,
    _capacity: OwnedSemaphorePermit,
}

impl Gate {
    fn new(capacity: OwnedSemaphorePermit, waiters_per_room: usize) -> Self {
        Self {
            mutex: Arc::new(AsyncMutex::new(())),
            waiters: Arc::new(Semaphore::new(waiters_per_room.saturating_add(1))),
            active: AtomicUsize::new(0),
            _capacity: capacity,
        }
    }
}

/// Keeps one caller's slot in the room's bounded wait queue.
#[derive(Debug)]
struct GateLease {
    _permit: OwnedSemaphorePermit,
    gate: Arc<Gate>,
    room_id: String,
    registry: Arc<GateRegistry>,
}

impl GateLease {
    fn new(
        gate: Arc<Gate>,
        waiters_per_room: usize,
        room_id: &str,
        registry: Arc<GateRegistry>,
    ) -> Result<Self, OutboundError> {
        let permit = Arc::clone(&gate.waiters).try_acquire_owned().map_err(|_| {
            OutboundError::GateOverflow {
                room_id: room_id.to_owned(),
                limit: waiters_per_room,
            }
        })?;
        gate.active.fetch_add(1, Ordering::AcqRel);
        Ok(Self {
            _permit: permit,
            gate,
            room_id: room_id.to_owned(),
            registry,
        })
    }
}

impl Drop for GateLease {
    fn drop(&mut self) {
        if self.gate.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            let mut gates = self.registry.lock();
            let removable = gates.get(&self.room_id).is_some_and(|gate| {
                Arc::ptr_eq(gate, &self.gate) && gate.active.load(Ordering::Acquire) == 0
            });
            if removable {
                gates.remove(&self.room_id);
            }
        }
    }
}

/// The per-room ordering guard returned by [`RoomGates::acquire`].
#[derive(Debug)]
pub(crate) struct GateGuard {
    _guard: OwnedMutexGuard<()>,
    _lease: GateLease,
}

/// Whether an acquire produced a gate or was allowed to proceed unordered.
#[derive(Debug)]
pub(crate) enum GateOutcome {
    /// The send acquired the room gate and must hold it for the request.
    Ordered(GateGuard),
    /// Saturation policy allowed the send to proceed without a gate.
    Unordered,
}

/// The shared bounded gate registry.
#[derive(Debug)]
struct GateRegistry {
    gates: Mutex<HashMap<String, Arc<Gate>>>,
    capacity: Arc<Semaphore>,
    capacity_limit: usize,
    saturation: OrderingSaturation,
    waiters_per_room: usize,
    wait_timeout: Duration,
    saturation_waits: AtomicUsize,
    unordered_sends: AtomicUsize,
}

impl GateRegistry {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<Gate>>> {
        self.gates.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One async mutex per room, created on demand and bounded by capacity.
#[derive(Debug)]
pub(crate) struct RoomGates {
    registry: Arc<GateRegistry>,
}

impl RoomGates {
    /// An empty gate registry with `capacity` slots.
    pub(crate) fn new(
        capacity: usize,
        saturation: OrderingSaturation,
    ) -> Result<Self, OutboundError> {
        if capacity == 0 {
            return Err(OutboundError::InvalidConfig {
                reason: "per-room gate capacity is 0, which refuses every gate".to_owned(),
            });
        }
        Ok(Self {
            registry: Arc::new(GateRegistry {
                gates: Mutex::new(HashMap::new()),
                capacity: Arc::new(Semaphore::new(capacity)),
                capacity_limit: capacity,
                saturation,
                waiters_per_room: GATE_WAITERS_PER_ROOM,
                wait_timeout: GATE_WAIT_TIMEOUT,
                saturation_waits: AtomicUsize::new(0),
                unordered_sends: AtomicUsize::new(0),
            }),
        })
    }

    #[cfg(test)]
    fn with_wait_limits(
        capacity: usize,
        saturation: OrderingSaturation,
        waiters_per_room: usize,
        wait_timeout: Duration,
    ) -> Result<Self, OutboundError> {
        if capacity == 0 {
            return Err(OutboundError::InvalidConfig {
                reason: "per-room gate capacity is 0, which refuses every gate".to_owned(),
            });
        }
        Ok(Self {
            registry: Arc::new(GateRegistry {
                gates: Mutex::new(HashMap::new()),
                capacity: Arc::new(Semaphore::new(capacity)),
                capacity_limit: capacity,
                saturation,
                waiters_per_room,
                wait_timeout,
                saturation_waits: AtomicUsize::new(0),
                unordered_sends: AtomicUsize::new(0),
            }),
        })
    }

    /// Number of active room gates, for tests and `Debug`.
    pub(crate) fn len(&self) -> usize {
        self.registry.lock().len()
    }

    /// How many sends waited because the gate map was saturated.
    pub(crate) fn saturation_waits(&self) -> usize {
        self.registry.saturation_waits.load(Ordering::Relaxed)
    }

    /// How many sends proceeded under explicit unordered saturation.
    pub(crate) fn unordered_sends(&self) -> usize {
        self.registry.unordered_sends.load(Ordering::Relaxed)
    }

    /// Waits until no other send is in flight for `room_id` and returns the
    /// guard that keeps it that way, or applies the configured saturation
    /// policy when the bounded registry is full.
    ///
    /// The registry lock is only held for lookup/insert/remove; no map lock is
    /// held across the room gate's `.await`.
    pub(crate) async fn acquire(&self, room_id: RoomKey<'_>) -> Result<GateOutcome, OutboundError> {
        match self.registry.saturation {
            OrderingSaturation::Backpressure => {
                let immediate = {
                    let mut gates = self.registry.lock();
                    if let Some(existing) = gates.get(room_id.as_str()) {
                        Some(Ok(Arc::clone(existing)))
                    } else if let Ok(permit) =
                        Arc::clone(&self.registry.capacity).try_acquire_owned()
                    {
                        let gate = Arc::new(Gate::new(permit, self.registry.waiters_per_room));
                        gates.insert(room_id.as_str().to_owned(), Arc::clone(&gate));
                        Some(Ok(gate))
                    } else {
                        None
                    }
                };
                if let Some(gate) = immediate {
                    return self.order(room_id, gate?).await;
                }

                self.registry
                    .saturation_waits
                    .fetch_add(1, Ordering::Relaxed);
                let permit = Arc::clone(&self.registry.capacity)
                    .acquire_owned()
                    .await
                    .map_err(|_| OutboundError::InvalidConfig {
                        reason: "per-room gate capacity was closed".to_owned(),
                    })?;
                let gate = {
                    let mut gates = self.registry.lock();
                    if let Some(existing) = gates.get(room_id.as_str()) {
                        Arc::clone(existing)
                    } else {
                        let gate = Arc::new(Gate::new(permit, self.registry.waiters_per_room));
                        gates.insert(room_id.as_str().to_owned(), Arc::clone(&gate));
                        gate
                    }
                };
                self.order(room_id, gate).await
            }
            OrderingSaturation::Error => {
                let gate = {
                    let mut gates = self.registry.lock();
                    if let Some(existing) = gates.get(room_id.as_str()) {
                        Some(Arc::clone(existing))
                    } else {
                        match Arc::clone(&self.registry.capacity).try_acquire_owned() {
                            Ok(permit) => {
                                let gate =
                                    Arc::new(Gate::new(permit, self.registry.waiters_per_room));
                                gates.insert(room_id.as_str().to_owned(), Arc::clone(&gate));
                                Some(gate)
                            }
                            Err(_) => None,
                        }
                    }
                };
                let Some(gate) = gate else {
                    return Err(OutboundError::OrderingLost {
                        room_id: room_id.as_str().to_owned(),
                        capacity: self.registry.capacity_limit,
                    });
                };
                self.order(room_id, gate).await
            }
            OrderingSaturation::Unordered => {
                let gate = {
                    let mut gates = self.registry.lock();
                    if let Some(existing) = gates.get(room_id.as_str()) {
                        Some(Arc::clone(existing))
                    } else {
                        match Arc::clone(&self.registry.capacity).try_acquire_owned() {
                            Ok(permit) => {
                                let gate =
                                    Arc::new(Gate::new(permit, self.registry.waiters_per_room));
                                gates.insert(room_id.as_str().to_owned(), Arc::clone(&gate));
                                Some(gate)
                            }
                            Err(_) => None,
                        }
                    }
                };
                if let Some(gate) = gate {
                    return self.order(room_id, gate).await;
                }
                self.registry
                    .unordered_sends
                    .fetch_add(1, Ordering::Relaxed);
                Ok(GateOutcome::Unordered)
            }
        }
    }

    async fn order(
        &self,
        room_id: RoomKey<'_>,
        gate: Arc<Gate>,
    ) -> Result<GateOutcome, OutboundError> {
        let lease = GateLease::new(
            Arc::clone(&gate),
            self.registry.waiters_per_room,
            room_id.as_str(),
            Arc::clone(&self.registry),
        )?;
        // If this acquire is cancelled while waiting, `lease` drops and the
        // room may be removed from the map. While it is held, the caller's lease
        // and tokio guard keep the room serialised.
        match tokio::time::timeout(
            self.registry.wait_timeout,
            Arc::clone(&gate.mutex).lock_owned(),
        )
        .await
        {
            Ok(guard) => Ok(GateOutcome::Ordered(GateGuard {
                _guard: guard,
                _lease: lease,
            })),
            Err(_) => Err(OutboundError::GateTimeout {
                room_id: room_id.as_str().to_owned(),
                waited: self.registry.wait_timeout,
            }),
        }
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
        RoomGates::new(capacity, OrderingSaturation::Backpressure).expect("a non-zero capacity")
    }

    fn gates_with(capacity: usize, saturation: OrderingSaturation) -> RoomGates {
        RoomGates::new(capacity, saturation).expect("a non-zero capacity")
    }

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        let mut context = Context::from_waker(Waker::noop());
        future.poll(&mut context)
    }

    async fn acquire_ordered(gates: &RoomGates, room: &str) -> GateGuard {
        match gates.acquire(key(room)).await.expect("a gate") {
            GateOutcome::Ordered(guard) => guard,
            GateOutcome::Unordered => panic!("expected an ordered gate"),
        }
    }

    #[test]
    fn gates_are_created_per_room_and_reused() {
        runtime().block_on(async {
            let gates = gates(GATE_CAPACITY);
            assert_eq!(gates.len(), 0);
            let first = acquire_ordered(&gates, "!a:x").await;
            assert_eq!(gates.len(), 1);
            drop(first);
            assert_eq!(gates.len(), 0);
            let second = acquire_ordered(&gates, "!a:x").await;
            assert_eq!(gates.len(), 1);
            let other = acquire_ordered(&gates, "!b:x").await;
            assert_eq!(gates.len(), 2);
            drop((second, other));
            assert_eq!(gates.len(), 0);
        });
    }

    #[test]
    fn one_room_serialises_and_two_rooms_do_not() {
        runtime().block_on(async {
            let gates = Arc::new(gates(GATE_CAPACITY));
            let held = acquire_ordered(&gates, "!a:x").await;
            let blocked = {
                let gates = Arc::clone(&gates);
                tokio::spawn(async move { acquire_ordered(&gates, "!a:x").await })
            };
            let free = gates.acquire(key("!b:x")).await;
            assert!(matches!(free, Ok(GateOutcome::Ordered(_))));
            tokio::time::sleep(Duration::from_millis(10)).await;
            assert!(!blocked.is_finished(), "the same room must block");
            drop(held);
            let blocked = blocked.await.expect("task");
            drop(blocked);
        });
    }

    #[test]
    fn a_full_registry_backpressures_new_rooms_until_a_gate_is_released() {
        runtime().block_on(async {
            let gates = Arc::new(gates(1));
            let held = acquire_ordered(&gates, "!a:x").await;
            let mut waiter = Box::pin(gates.acquire(key("!b:x")));

            assert!(poll_once(waiter.as_mut()).is_pending());
            assert_eq!(gates.len(), 1);
            assert_eq!(gates.saturation_waits(), 1);

            drop(held);
            let outcome = match poll_once(waiter.as_mut()) {
                Poll::Ready(outcome) => outcome,
                Poll::Pending => panic!("the released gate must admit the waiter"),
            };
            assert!(matches!(outcome, Ok(GateOutcome::Ordered(_))));
            assert_eq!(gates.len(), 1);
            drop(outcome);
            assert_eq!(gates.len(), 0);
        });
    }

    #[test]
    fn zero_capacity_is_rejected() {
        let error = RoomGates::new(0, OrderingSaturation::Backpressure)
            .expect_err("capacity 0 is unusable");

        assert!(matches!(error, OutboundError::InvalidConfig { .. }));
        assert!(error.to_string().contains("capacity is 0"), "{error}");
    }

    #[test]
    fn capacity_one_keeps_the_map_bounded_and_serialises_the_room() {
        runtime().block_on(async {
            let gates = gates(1);
            let first = acquire_ordered(&gates, "!a:x").await;
            assert_eq!(gates.len(), 1);

            let mut waiter = Box::pin(gates.acquire(key("!a:x")));
            assert!(poll_once(waiter.as_mut()).is_pending());
            assert_eq!(gates.len(), 1);

            drop(first);
            let outcome = match poll_once(waiter.as_mut()) {
                Poll::Ready(outcome) => outcome,
                Poll::Pending => panic!("the released gate must admit the waiter"),
            };
            assert!(matches!(outcome, Ok(GateOutcome::Ordered(_))));
            assert_eq!(gates.len(), 1);
            drop(outcome);
            assert_eq!(gates.len(), 0);
        });
    }

    #[test]
    fn capacity_is_an_exact_boundary_not_an_exclusive_one() {
        runtime().block_on(async {
            let gates = gates(2);
            let first = acquire_ordered(&gates, "!a:x").await;
            let second = acquire_ordered(&gates, "!b:x").await;
            assert_eq!(gates.len(), 2);

            let mut third = Box::pin(gates.acquire(key("!c:x")));
            assert!(poll_once(third.as_mut()).is_pending());
            assert_eq!(gates.len(), 2);

            drop(first);
            let outcome = match poll_once(third.as_mut()) {
                Poll::Ready(outcome) => outcome,
                Poll::Pending => panic!("the released gate must admit the waiter"),
            };
            assert!(matches!(outcome, Ok(GateOutcome::Ordered(_))));
            assert_eq!(gates.len(), 2);
            drop((second, outcome));
            assert_eq!(gates.len(), 0);
        });
    }

    #[test]
    fn a_cancelled_waiter_does_not_block_the_room() {
        runtime().block_on(async {
            let gates = gates(1);
            let held = acquire_ordered(&gates, "!queued:x").await;
            let mut waiter = Box::pin(gates.acquire(key("!queued:x")));

            assert!(poll_once(waiter.as_mut()).is_pending());
            drop(waiter);

            drop(held);
            let new = gates.acquire(key("!queued:x")).await;
            assert!(matches!(new, Ok(GateOutcome::Ordered(_))));
            assert_eq!(gates.len(), 1);
        });
    }

    #[test]
    fn one_room_queue_admits_the_limit_and_rejects_the_next_waiter() {
        runtime().block_on(async {
            let gates = RoomGates::with_wait_limits(
                1,
                OrderingSaturation::Backpressure,
                2,
                GATE_WAIT_TIMEOUT,
            )
            .expect("valid limits");
            let held = gates.acquire(key("!queued:x")).await.expect("a gate");
            let mut first = Box::pin(gates.acquire(key("!queued:x")));
            let mut second = Box::pin(gates.acquire(key("!queued:x")));

            assert!(poll_once(first.as_mut()).is_pending());
            assert!(poll_once(second.as_mut()).is_pending());

            let error = gates
                .acquire(key("!queued:x"))
                .await
                .expect_err("the queue is full");
            assert!(matches!(
                error,
                OutboundError::GateOverflow { limit: 2, .. }
            ));

            drop(held);
        });
    }

    #[test]
    fn a_waiter_times_out_instead_of_waiting_forever() {
        runtime().block_on(async {
            let gates = RoomGates::with_wait_limits(
                1,
                OrderingSaturation::Backpressure,
                1,
                Duration::from_millis(5),
            )
            .expect("valid limits");
            let held = gates.acquire(key("!queued:x")).await.expect("a gate");

            let error = gates
                .acquire(key("!queued:x"))
                .await
                .expect_err("the gate must time out");
            assert!(matches!(
                error,
                OutboundError::GateTimeout { waited, .. } if waited == Duration::from_millis(5)
            ));

            drop(held);
        });
    }

    #[test]
    fn a_timed_out_waiter_releases_its_queue_slot() {
        runtime().block_on(async {
            let gates = RoomGates::with_wait_limits(
                1,
                OrderingSaturation::Backpressure,
                1,
                Duration::from_millis(5),
            )
            .expect("valid limits");
            let held = gates.acquire(key("!queued:x")).await.expect("a gate");

            let error = gates
                .acquire(key("!queued:x"))
                .await
                .expect_err("the gate must time out");
            assert!(matches!(error, OutboundError::GateTimeout { .. }));

            let mut waiter = Box::pin(gates.acquire(key("!queued:x")));
            assert!(poll_once(waiter.as_mut()).is_pending());
            drop(held);
            assert!(poll_once(waiter.as_mut()).is_ready());
        });
    }

    #[test]
    fn strict_saturation_returns_ordering_lost_at_the_boundary() {
        runtime().block_on(async {
            let gates = gates_with(1, OrderingSaturation::Error);
            let held = acquire_ordered(&gates, "!a:x").await;
            let error = gates
                .acquire(key("!b:x"))
                .await
                .expect_err("the second room is beyond capacity");

            assert!(matches!(error, OutboundError::OrderingLost { .. }));
            assert_eq!(
                error,
                OutboundError::OrderingLost {
                    room_id: "!b:x".to_owned(),
                    capacity: 1,
                }
            );
            drop(held);
        });
    }

    #[test]
    fn explicit_unordered_saturation_is_observable() {
        runtime().block_on(async {
            let gates = gates_with(1, OrderingSaturation::Unordered);
            let held = acquire_ordered(&gates, "!a:x").await;
            let outcome = gates.acquire(key("!b:x")).await.expect("explicit fallback");

            assert!(matches!(outcome, GateOutcome::Unordered));
            assert_eq!(gates.unordered_sends(), 1);
            assert_eq!(gates.saturation_waits(), 0);
            drop(held);
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
        fn unordered_gate_count_never_exceeds_capacity(capacity in 1usize..8, rooms in 1usize..24) {
            let result: Result<(), TestCaseError> = runtime().block_on(async {
                let gates = gates_with(capacity, OrderingSaturation::Unordered);
                let mut held = Vec::new();
                for index in 0..rooms {
                    let room = format!("!{index}:x");
                    match gates.acquire(key(&room)).await {
                        Ok(GateOutcome::Ordered(gate)) => held.push(gate),
                        Ok(GateOutcome::Unordered) => {}
                        Err(error) => return Err(TestCaseError::fail(error.to_string())),
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
                    let mut extra_waiter = Box::pin(gates.acquire(key(&room)));
                    prop_assert!(poll_once(extra_waiter.as_mut()).is_pending());
                    prop_assert!(gates.len() <= capacity);
                }

                prop_assert!(gates.len() <= capacity);
                Ok(())
            });
            prop_assert!(result.is_ok(), "{result:?}");
        }
    }
}
