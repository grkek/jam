//! Per-core round-robin scheduling with generation-aware actor queue entries.
//!
//! Foundation's [`LocalAddress`] run queue stores actor slot and generation
//! directly. Actor state and queue membership are synchronized by the
//! actor control-block lock before publishing an entry.
//!
//! **Lock order:** actor control block -> one core run queue. Work stealing
//! locks two run queues in ascending core-index order but never takes an
//! actor lock. Dispatch drops the queue lock before claiming an actor.
//! Actors marked `Exiting` are never dispatched or requeued.

use foundation::address::LocalAddress;

use super::core_local::{CoreLocal, NO_ACTOR, core_locals, current_core_local};
use crate::{
    actors::{
        control_block::ActorState,
        table::{ACTOR_MAXIMUM, with_control_block},
    },
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

/// Publishes a runnable actor exactly once to the calling core's run queue.
///
/// A duplicate enqueue is ignored. Failure to find a current core, a full
/// queue, or an actor in any state other than `Runnable` leaves queue
/// membership unchanged. This is also used when spawning an actor.
pub fn enqueue_runnable(slot_index: usize) -> bool {
    let Some(core_local) = current_core_local() else {
        return false;
    };
    with_control_block(slot_index, |actor| {
        if actor.state != ActorState::Runnable {
            return false;
        }

        if actor.queued {
            return true;
        }

        let Ok(n) = u32::try_from(slot_index) else {
            return false;
        };

        let mut queue = core_local.run_queue().lock();

        if queue.length() >= ACTOR_MAXIMUM {
            return false;
        }

        let ok = queue.push_back(LocalAddress::new(n, actor.generation_counter));
        actor.queued = ok;

        ok
    })
    .unwrap_or(false)
}

/// Balances runnable actors with the busiest online core.
///
/// Locks the two run queues in ascending core-index order, so transfers cannot
/// drop an entry or deadlock with another simultaneous work-stealing attempt.
/// Actor locks are never acquired while holding these queue locks.
fn rebalance(core_local: &CoreLocal) {
    let own_length = core_local.run_queue().lock().length();
    let mut busiest: Option<(&CoreLocal, usize)> = None;

    for other_core in core_locals() {
        if other_core.core_index() == core_local.core_index() || !other_core.is_online() {
            continue;
        }

        let length = other_core.run_queue().lock().length();

        if busiest.is_none_or(|(_, longest)| length > longest) {
            busiest = Some((other_core, length));
        }
    }

    let Some((victim, victim_length)) = busiest else {
        return;
    };

    if victim_length <= own_length + 1 {
        return;
    }

    let (first, second) = if core_local.core_index() < victim.core_index() {
        (core_local, victim)
    } else {
        (victim, core_local)
    };

    let mut first_queue = first.run_queue().lock();
    let mut second_queue = second.run_queue().lock();

    let (own_queue, victim_queue) = if core_local.core_index() < victim.core_index() {
        (&mut *first_queue, &mut *second_queue)
    } else {
        (&mut *second_queue, &mut *first_queue)
    };

    let own_count = own_queue.length();
    let victim_count = victim_queue.length();

    if victim_count <= own_count + 1 {
        return;
    }

    let transfer_count = ((victim_count - own_count) / 2)
        .min(STEAL_BATCH_MAXIMUM)
        .min(ACTOR_MAXIMUM - own_count);

    for _ in 0..transfer_count {
        let Some(entry) = victim_queue.pop_back() else {
            break;
        };
        // We reserved capacity above while holding both run-queue locks.
        let inserted = own_queue.push_back(entry);
        assert!(inserted, "reserved destination capacity before stealing");
    }
}

/// Atomically claims a queued actor only if its identity and state still match.
fn claim_next_runnable(core_local: &CoreLocal) -> Option<(usize, usize, Option<(u64, u16)>)> {
    loop {
        let address = core_local.run_queue().lock().pop_front()?;
        let slot_index = address.slot_index() as usize;

        let claimed = with_control_block(slot_index, |actor| {
            if actor.generation_counter != address.generation_counter() {
                // Never touch new-incarnation bookkeeping for an old entry.
                return None;
            }

            if actor.state != ActorState::Runnable {
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
                actor.state = ActorState::Exiting;
                return None;
            }

            actor.queued = false;
            actor.state = ActorState::Running;

            let mapping = actor.address_space.as_ref().map(|space| {
                (
                    space.root_table_address() as u64,
                    actor.address_space_identifier,
                )
            });

            Some((actor.saved_frame_address, mapping))
        })
        .flatten();

        if let Some((frame, mapping)) = claimed {
            return Some((slot_index, frame, mapping));
        }
    }
}

/// Returns the next actor frame or the idle frame, updating the current core.
fn dispatch_next(core_local: &CoreLocal) -> usize {
    rebalance(core_local);
    match claim_next_runnable(core_local) {
        Some((slot_index, frame_address, lower_half)) => {
            match lower_half {
                Some((root, identifier)) => activate_lower_half(root as _, identifier),
                None => deactivate_lower_half(),
            }
            core_local.set_current_slot_index(slot_index);
            validate_resume_frame(core_local, frame_address);
            frame_address
        }
        None => {
            core_local.set_current_slot_index(NO_ACTOR);
            deactivate_lower_half();

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

/// Saves an interrupted execution context before publishing it for scheduling.
///
/// Runs from the per-core interrupt stack. The actor can only be stolen after
/// its saved frame has been committed and it has left `Running`.
fn schedule(core_local: &CoreLocal, interrupted_frame_address: usize) -> usize {
    let current_slot = core_local.current_slot_index();

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
            if actor.state != ActorState::Running {
                // Waiting and Exiting actors are never requeued.
                return;
            }
            let Ok(slot_number) = u32::try_from(current_slot) else {
                return;
            };
            let mut queue = core_local.run_queue().lock();
            if queue.length() >= ACTOR_MAXIMUM {
                queue_was_full = true;
                return;
            }
            let address = LocalAddress::new(slot_number, actor.generation_counter);
            // Leave this core's current slot before another core can steal
            // and finish this actor. We remain on the interrupt stack.
            core_local.set_current_slot_index(NO_ACTOR);
            // Commit the saved frame *before* publishing its run queue entry.
            actor.state = ActorState::Runnable;
            actor.queued = true;
            let inserted = queue.push_back(address);

            assert!(inserted, "checked run queue capacity under lock");
        });
        if queue_was_full {
            // The interrupted actor is still Running and never left the core.
            return interrupted_frame_address;
        }
    }

    // This core no longer owns the saved actor. Another core may steal it.
    core_local.set_current_slot_index(NO_ACTOR);

    dispatch_next(core_local)
}

/// Picks the next runnable frame after `FinishActor` relinquishes its core.
#[must_use]
pub fn pick_next_after_finish(core_local: &CoreLocal) -> usize {
    dispatch_next(core_local)
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

    schedule(core_local, interrupted_frame_address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_queue_entries_preserve_actor_generation() {
        let address = LocalAddress::new(17, 0xabcdef12);
        let mut queue = foundation::run_queue::RunQueue::<1>::new();
        assert!(queue.push_back(address));
        assert_eq!(queue.pop_front(), Some(address));
    }
}
