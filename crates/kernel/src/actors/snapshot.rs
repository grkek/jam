
 //! Constant-time diagnostic counters for actor lifecycle transitions.
 //!
 //! Counters are updated while holding the affected actor's control-block
 //! lock. Independent reads are diagnostic, not an atomic global snapshot.

 use core::sync::atomic::{AtomicUsize, Ordering};

 use super::control_block::State;

 static OCCUPIED_ACTOR_COUNT: AtomicUsize = AtomicUsize::new(0);
 static WAITING_ACTOR_COUNT: AtomicUsize = AtomicUsize::new(0);

 /// Records one actor-state change.
 ///
 /// Must be called exactly once for each real transition, while the
 /// caller exclusively owns the actor's control block.
 pub(crate) fn record_transition(previous: State, next: State) {
     if previous == next {
         return;
     }

     if previous == State::Free {
         OCCUPIED_ACTOR_COUNT.fetch_add(1, Ordering::Relaxed);
     } else if next == State::Free {
         OCCUPIED_ACTOR_COUNT.fetch_sub(1, Ordering::Relaxed);
     }

     if previous == State::Waiting {
         WAITING_ACTOR_COUNT.fetch_sub(1, Ordering::Relaxed);
     } else if next == State::Waiting {
         WAITING_ACTOR_COUNT.fetch_add(1, Ordering::Relaxed);
     }
 }

 /// Returns independently sampled occupancy counters.
 #[must_use]
 pub fn current() -> (usize, usize) {
     (
         OCCUPIED_ACTOR_COUNT.load(Ordering::Relaxed),
         WAITING_ACTOR_COUNT.load(Ordering::Relaxed),
     )
 }
