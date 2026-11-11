//! Per-core round-robin scheduling with generation-aware actor queue entries.
//!
//! Foundation's [`LocalAddress`] queue stores actor slot and generation
//! directly. Actor state and queue membership are synchronized by the
//! actor control-block lock before publishing an entry.
//!
//! **Lock order:** actor control block -> one core queue. Work stealing
//! locks two queues in ascending core-index order but never takes an
//! actor lock. Dispatch drops the queue lock before claiming an actor.
//! Actors marked `Exiting` are never dispatched or requeued.

use foundation::address::LocalAddress;

use super::core_local::{CoreLocal, NO_ACTOR, core_locals, current_core_local};
use crate::{
    actors::{control_block::State, table::with_control_block},
    architecture::aarch64::{
        exceptions::TrapFrame,
        generic_timer::{rearm_tick, start_tick},
        interrupt_controller::initialise_core_interface,
        memory_management_unit::{activate_lower_half, deactivate_lower_half},
        processor::enable_interrupts,
        time,
    },
};

/// Maximum number of runnable actors transferred in one balancing operation.
const STEAL_BATCH_MAXIMUM: usize = 16;

/// Number of scheduler timer interrupts per second, on each core.
const TICKS_PER_SECOND: u64 = 100;

/// Probe for sustained cross-core imbalance every tenth tick (100 milliseconds).
const BALANCE_CHECK_INTERVAL_TICKS: u64 = 10;

/// Maximum excess load tolerated before overriding the last-core affinity hint.
const AFFINITY_LOAD_TOLERANCE: usize = 1;

/// Returns the machine counter delta between scheduler ticks.
fn tick_interval() -> u64 {
    time::read_counter_frequency() / TICKS_PER_SECOND
}

/// Starts periodic preemption and enables interrupts on the calling core.
pub fn start_current_core_tick() {
    initialise_core_interface();
    start_tick(tick_interval());
    enable_interrupts();
}

/// Returns whether the previous core is sufficiently lightly loaded for cache affinity.
fn preserve_cache_affinity(
    previous_core_is_online: bool,
    previous_core_load: usize,
    current_core_load: usize,
) -> bool {
    previous_core_is_online
        && previous_core_load <= current_core_load.saturating_add(AFFINITY_LOAD_TOLERANCE)
}

/// Selects an eligible queue using a soft previous-core hint, never a hard affinity rule.
///
/// Load estimates are advisory. The chosen queue is always locked before publishing work.
fn choose_enqueue_core(
    current_core: &'static CoreLocal,
    last_core_index: Option<usize>,
) -> &'static CoreLocal {
    let Some(previous_core) = last_core_index.and_then(|index| core_locals().get(index)) else {
        return current_core;
    };

    if previous_core.core_index() == current_core.core_index() {
        return current_core;
    }

    if preserve_cache_affinity(
        previous_core.is_online(),
        previous_core.load_estimate(),
        current_core.load_estimate(),
    ) {
        previous_core
    } else {
        current_core
    }
}

/// Publishes a runnable actor exactly once to the selected core's queue.
///
/// A duplicate enqueue is ignored. Failure to find a current core, a full
/// queue, or an actor in any state other than `Runnable` leaves queue
/// membership unchanged. This is also used when spawning an actor.
pub fn enqueue_runnable(slot_index: usize) -> bool {
    let Some(current_core) = current_core_local() else {
        return false;
    };
    with_control_block(slot_index, |actor| {
        if actor.state != State::Runnable {
            return false;
        }

        if actor.queued {
            return true;
        }

        let Ok(n) = u32::try_from(slot_index) else {
            return false;
        };

        let selected_core = choose_enqueue_core(current_core, actor.last_core_index);
        let mut queue = selected_core.queue().lock();

        if queue.length() >= queue.capacity() {
            return false;
        }

        let inserted = queue.push_back(LocalAddress::new(n, actor.generation_counter));
        if inserted {
            selected_core.publish_queue_length(queue.length());
        }
        actor.queued = inserted;

        inserted
    })
    .unwrap_or(false)
}

/// Returns whether a remote queue seems sufficiently busier to make stealing useful.
///
/// Approximate lengths are advisory; the actual transfer rechecks both locked queues.
fn has_stealable_work(core_local: &CoreLocal) -> bool {
    let minimum_victim_length = core_local.queue_length_estimate().saturating_add(1);
    core_locals().iter().any(|candidate| {
        candidate.core_index() != core_local.core_index()
            && candidate.is_online()
            && candidate.queue_length_estimate() > minimum_victim_length
    })
}

/// Balances runnable actors with the busiest online core, only when requested.
///
/// A relaxed-atomic scan replaces the previous N-core queue-lock scan. The only
/// queue locks acquired are the destination and selected victim, in ascending
/// core-index order. No actor lock is acquired while holding either queue lock.
fn rebalance(core_local: &CoreLocal) {
    core_local.record_steal_attempt();
    let own_length = core_local.queue_length_estimate();
    let mut busiest: Option<(&CoreLocal, usize)> = None;

    for candidate in core_locals() {
        if candidate.core_index() == core_local.core_index() || !candidate.is_online() {
            continue;
        }
        let candidate_length = candidate.queue_length_estimate();
        if candidate_length > own_length.saturating_add(1)
            && busiest.is_none_or(|(_, longest)| candidate_length > longest)
        {
            busiest = Some((candidate, candidate_length));
        }
    }

    let Some((victim, _)) = busiest else {
        return;
    };

    let (first, second) = if core_local.core_index() < victim.core_index() {
        (core_local, victim)
    } else {
        (victim, core_local)
    };

    let mut first_queue = first.queue().lock();
    let mut second_queue = second.queue().lock();

    let (own_queue, victim_queue) = if core_local.core_index() < victim.core_index() {
        (&mut *first_queue, &mut *second_queue)
    } else {
        (&mut *second_queue, &mut *first_queue)
    };

    let own_count = own_queue.length();
    let victim_count = victim_queue.length();
    if victim_count <= own_count.saturating_add(1) {
        return;
    }

    let transfer_count = ((victim_count - own_count) / 2)
        .min(STEAL_BATCH_MAXIMUM)
        .min(own_queue.capacity().saturating_sub(own_count));

    let mut transferred_count = 0;
    for _ in 0..transfer_count {
        let Some(entry) = victim_queue.pop_back() else {
            break;
        };
        // The destination's capacity was reserved while both queues were locked.
        assert!(
            own_queue.push_back(entry),
            "Reserved destination capacity before stealing"
        );
        transferred_count += 1;
    }

    core_local.publish_queue_length(own_queue.length());
    victim.publish_queue_length(victim_queue.length());
    core_local.record_stolen_actors(transferred_count);
}

/// Atomically claims a queued actor only if its identity and state still match.
fn claim_next_runnable(core_local: &CoreLocal) -> Option<(usize, usize, Option<(u64, u16)>, bool)> {
    loop {
        let address = {
            let mut queue = core_local.queue().lock();
            let address = queue.pop_front()?;
            core_local.publish_queue_length(queue.length());
            address
        };
        let slot_index = address.slot_index() as usize;

        let claimed = with_control_block(slot_index, |actor| {
            if actor.generation_counter != address.generation_counter() {
                // Never touch new-incarnation bookkeeping for an old entry.
                return None;
            }

            if actor.state != State::Runnable {
                // The popped entry is gone; clear membership for this
                // incarnation, including if the actor has become Exiting.
                actor.queued = false;
                return None;
            }

            // A Runnable actor popped from a queue must own a queue entry and a saved frame.
            assert!(actor.queued && actor.saved_frame_address != 0);

            assert_ne!(
                unsafe {
                    core::ptr::read_volatile((actor.saved_frame_address + 248) as *const u64)
                },
                0,
                "dispatching frame with ELR 0"
            );

            if !actor.queued || actor.saved_frame_address == 0 {
                // Release builds: refuse to run it, but don't strand it silently.
                actor.transition_to(State::Exiting);
                return None;
            }

            actor.queued = false;
            actor.transition_to(State::Running);
            let migrated = actor
                .last_core_index
                .is_some_and(|previous| previous != core_local.core_index());
            actor.last_core_index = Some(core_local.core_index());

            let mapping = actor.address_space.as_ref().map(|space| {
                (
                    space.root_table_address() as u64,
                    actor.address_space_identifier,
                )
            });

            Some((actor.saved_frame_address, mapping, migrated))
        })
        .flatten();

        if let Some((frame, mapping, migrated)) = claimed {
            return Some((slot_index, frame, mapping, migrated));
        }
    }
}

/// Returns the next actor frame, favoring local work before optional stealing.
fn dispatch_next(core_local: &CoreLocal, periodic_balance: bool) -> usize {
    // Demand-driven stealing when empty, with a periodic path to correct
    // sustained imbalance even if every core runs at least one actor.
    if periodic_balance {
        rebalance(core_local);
    }
    let mut next_actor = claim_next_runnable(core_local);
    if next_actor.is_none() && !periodic_balance {
        rebalance(core_local);
        next_actor = claim_next_runnable(core_local);
    }

    match next_actor {
        Some((slot_index, frame_address, lower_half, migrated)) => {
            match lower_half {
                Some((root, identifier)) => activate_lower_half(root as _, identifier),
                None => deactivate_lower_half(),
            }
            core_local.set_current_slot_index(slot_index);
            core_local.record_actor_dispatch(migrated);
            validate_resume_frame(core_local, frame_address);
            frame_address
        }
        None => {
            core_local.set_current_slot_index(NO_ACTOR);
            deactivate_lower_half();
            core_local.record_idle_dispatch();
            validate_resume_frame(core_local, core_local.idle_frame_address())
        }
    }
}

fn validate_resume_frame(core_local: &CoreLocal, frame_address: usize) -> usize {
    assert_ne!(frame_address, 0, "Scheduler selected a null frame");

    assert_eq!(
        frame_address % align_of::<TrapFrame>(),
        0,
        "Scheduler selected a misaligned frame"
    );

    // SAFETY: The scheduler must guarantee that the selected
    // address points to a live, initialised TrapFrame.
    let frame = unsafe { &*(frame_address as *const TrapFrame) };

    assert_ne!(
        frame.exception_link_register, 0,
        "Scheduler selected a zero instruction pointer"
    );

    if core_local.current_slot_index() == NO_ACTOR {
        assert_eq!(
            frame.saved_program_status_register & 0xF,
            0x5,
            "Idle core is about to resume outside EL1h"
        );
    }

    frame_address
}

/// Avoids an unnecessary enqueue/dequeue and address-space switch on solo ticks.
///
/// Queue emptiness is verified under its own lock. Remote imbalance is sampled
/// only once per balancing interval; the next timer tick still provides preemption.
fn can_resume_current_actor_without_dispatch(
    core_local: &CoreLocal,
    current_slot_index: usize,
    periodic_balance: bool,
) -> bool {
    if current_slot_index == NO_ACTOR || !core_local.queue().lock().is_empty() {
        return false;
    }
    if periodic_balance && has_stealable_work(core_local) {
        return false;
    }
    with_control_block(current_slot_index, |actor| actor.state == State::Running).unwrap_or(false)
}

/// Saves an interrupted execution context before publishing it for scheduling.
///
/// Runs from the per-core interrupt stack. The actor can only be stolen after
/// its saved frame has been committed and it has left `Running`.
fn schedule(
    core_local: &CoreLocal,
    interrupted_frame_address: usize,
    periodic_balance: bool,
) -> usize {
    let current_slot = core_local.current_slot_index();

    if can_resume_current_actor_without_dispatch(core_local, current_slot, periodic_balance) {
        return interrupted_frame_address;
    }

    if current_slot == NO_ACTOR {
        // SAFETY: interrupted_frame_address is the TrapFrame the vector just saved; SPSR is at +256.
        let spsr =
            unsafe { core::ptr::read_volatile((interrupted_frame_address + 256) as *const u64) };
        if spsr & 0xf == 0 {
            crate::architecture::aarch64::power_interface::halt_machine();
        }
    }

    if current_slot == NO_ACTOR {
        core_local.set_idle_frame_address(interrupted_frame_address);
    } else {
        let mut queue_was_full = false;
        let _ = with_control_block(current_slot, |actor| {
            actor.saved_frame_address = interrupted_frame_address;
            if actor.state != State::Running {
                // Waiting and Exiting actors are never requeued.
                return;
            }
            let Ok(slot_number) = u32::try_from(current_slot) else {
                return;
            };
            let mut queue = core_local.queue().lock();
            if queue.length() >= queue.capacity() {
                queue_was_full = true;
                return;
            }
            let address = LocalAddress::new(slot_number, actor.generation_counter);
            // Leave this core's current slot before another core can steal
            // and finish this actor. We remain on the interrupt stack.
            core_local.set_current_slot_index(NO_ACTOR);
            // Commit the saved frame *before* publishing its queue entry.
            actor.transition_to(State::Runnable);
            actor.queued = true;
            let inserted = queue.push_back(address);
            assert!(inserted, "Checked queue capacity under lock");
            core_local.publish_queue_length(queue.length());
        });
        if queue_was_full {
            // The interrupted actor is still Running and never left the core.
            return interrupted_frame_address;
        }
    }

    // This core no longer owns the saved actor. Another core may steal it.
    core_local.set_current_slot_index(NO_ACTOR);

    dispatch_next(core_local, periodic_balance)
}

/// Picks the next runnable frame after `FinishActor` relinquishes its core.
#[must_use]
pub fn pick_next_after_finish(core_local: &CoreLocal) -> usize {
    dispatch_next(core_local, false)
}

/// Handles the timer interrupt and returns the next frame to resume.
#[must_use]
pub fn on_timer_tick(interrupted_frame_address: usize) -> usize {
    rearm_tick(tick_interval());
    let Some(core_local) = current_core_local() else {
        return interrupted_frame_address;
    };
    let tick = core_local.increment_tick_count();

    if tick % 100 == 0 {
        let (used, capacity) =
            crate::scheduling::idle::interrupt_stack_high_water_bytes(core_local.core_index());

        assert!(
            used < capacity - 4096,
            "Interrupt stack critically close to overflowing"
        );
    }

    schedule(
        core_local,
        interrupted_frame_address,
        tick % BALANCE_CHECK_INTERVAL_TICKS == 0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affinity_is_bounded_by_the_current_core_load() {
        assert!(preserve_cache_affinity(true, 3, 2));
        assert!(!preserve_cache_affinity(true, 4, 2));
        assert!(!preserve_cache_affinity(false, 0, 10));
    }

    #[test]
    fn queue_entries_preserve_actor_generation() {
        let address = LocalAddress::new(17, 0xabcdef12);
        let mut queue = foundation::queue::Queue::<1>::new();
        assert!(queue.push_back(address));
        assert_eq!(queue.pop_front(), Some(address));
    }
}

/// Publishes a messaging wake while the caller holds the destination actor lock.
///
/// Uses actor -> run-queue lock order, never recursively acquires the actor lock.
/// The queue has one entry per actor at most; a capacity failure is corruption.
/// SystemCall/interrupt callers must have interrupts masked until returning a frame.
pub fn wake_inbox_actor(
    actor: &mut crate::actors::control_block::ControlBlock,
    token: crate::actors::inbox::WakeToken,
) {
    use crate::architecture::aarch64::power_interface::halt_machine;
    if actor.generation_counter != token.actor_address().generation_counter()
        || actor.state != State::Waiting
        || actor.queued
    {
        return;
    }
    let Some(current_core) = current_core_local() else {
        halt_machine();
    };
    let selected_core = choose_enqueue_core(current_core, actor.last_core_index);
    let Some(inbox) = actor.inbox.as_mut() else {
        halt_machine();
    };
    let mut queue = selected_core.queue().lock();
    if queue.length() >= queue.capacity() {
        halt_machine();
    }
    if !inbox.claim_wake(token) {
        return;
    }
    actor.transition_to(State::Runnable);
    actor.queued = true;
    if !queue.push_back(token.actor_address()) {
        halt_machine();
    }
    selected_core.publish_queue_length(queue.length());
    drop(queue);
    crate::architecture::aarch64::interrupt_controller::request_reschedule(
        selected_core.core_index(),
    );
}

/// Dispatches after an inbox wait relinquished this core under the actor lock.
///
/// The old frame must not be accessed: another core may already be resuming it.
#[must_use]
pub fn pick_next_after_wait(core: &CoreLocal) -> usize {
    dispatch_next(core, false)
}

/// Dispatches published local work without rearming the timer or counting a tick.
#[must_use]
pub fn on_reschedule_interrupt(interrupted_frame_address: usize) -> usize {
    let Some(core_local) = current_core_local() else {
        return interrupted_frame_address;
    };
    // Polling-driver notifications need only return from WFI when no work waits.
    if core_local.queue().lock().is_empty() {
        return interrupted_frame_address;
    }
    schedule(core_local, interrupted_frame_address, false)
}

/// Notifies a running kernel actor participating in the polling sleep handshake.
/// The caller holds its actor lock, so its current core cannot change here.
pub fn notify_polling_actor(actor: &crate::actors::control_block::ControlBlock) {
    if actor.state != State::Running {
        return;
    }
    let Some(core_local) = actor
        .last_core_index
        .and_then(|index| core_locals().get(index))
    else {
        return;
    };
    if core_local.is_polling_inbox() {
        crate::architecture::aarch64::interrupt_controller::request_reschedule(
            core_local.core_index(),
        );
    }
}
