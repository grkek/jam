//! The scheduler: round-robin on each core, with idle cores stealing work.
//!
//! The timer interrupt saves the running context as a frame, picks the next actor, and returns
//! that actor's frame to resume. A context is only ever made runnable after its frame is saved,
//! and the handler runs on the core's own interrupt stack, so another core can resume the actor
//! the moment it is queued.

use super::core_local::{CoreLocal, NO_ACTOR, core_locals, current_core_local};
use crate::{
    actors::{control_block::ActorState, table::with_control_block},
    architecture::aarch64::{
        generic_timer::{rearm_tick, start_tick},
        interrupt_controller::initialise_core_interface,
        memory_management_unit::{activate_lower_half, deactivate_lower_half},
        processor::enable_interrupts,
        time,
    },
};

/// Most actors one theft takes at once.
const STEAL_BATCH_MAXIMUM: usize = 16;

/// How often the timer interrupts each core.
const TICKS_PER_SECOND: u64 = 100;

fn tick_interval() -> u64 {
    time::read_counter_frequency() / TICKS_PER_SECOND
}

/// Prepares the calling core for ticks and starts taking interrupts. Every core calls this once.
pub fn start_current_core_tick() {
    initialise_core_interface();
    start_tick(tick_interval());
    enable_interrupts();
}

/// Makes the actor in `slot_index` ready to run on the calling core.
pub fn enqueue_runnable(slot_index: usize) {
    if let Some(core_local) = current_core_local() {
        let _ = core_local.run_queue().lock().push_back(slot_index);
    }
}

/// Moves work from the core with the longest queue to this one, until the two are about equal.
///
/// Only one queue lock is held at a time, so two cores balancing against each other never
/// wait on each other's locks.
fn rebalance(core_local: &CoreLocal) {
    let own_length = core_local.run_queue().lock().length();

    let mut busiest: Option<(&CoreLocal, usize)> = None;
    for other_core_local in core_locals() {
        if other_core_local.core_index() == core_local.core_index() || !other_core_local.is_online()
        {
            continue;
        }
        let length = other_core_local.run_queue().lock().length();
        if busiest.is_none_or(|(_, longest)| length > longest) {
            busiest = Some((other_core_local, length));
        }
    }
    let Some((victim, victim_length)) = busiest else {
        return;
    };
    if victim_length <= own_length + 1 {
        return;
    }

    let steal_count = ((victim_length - own_length) / 2).min(STEAL_BATCH_MAXIMUM);
    let mut stolen_slot_indices = [0_usize; STEAL_BATCH_MAXIMUM];
    let mut stolen_count = 0;
    {
        let mut victim_queue = victim.run_queue().lock();
        while stolen_count < steal_count {
            let Some(slot_index) = victim_queue.pop_back() else {
                break;
            };
            if let Some(cell) = stolen_slot_indices.get_mut(stolen_count) {
                *cell = slot_index;
            }
            stolen_count += 1;
        }
    }

    let mut own_queue = core_local.run_queue().lock();
    for slot_index in stolen_slot_indices.iter().take(stolen_count) {
        let _ = own_queue.push_back(*slot_index);
    }
}

/// Saves the interrupted context and returns the frame of the context to run next.
fn schedule(core_local: &CoreLocal, interrupted_frame_address: usize) -> usize {
    let current_slot_index = core_local.current_slot_index();
    if current_slot_index == NO_ACTOR {
        core_local.set_idle_frame_address(interrupted_frame_address);
    } else {
        let _ = with_control_block(current_slot_index, |control_block| {
            control_block.saved_frame_address = interrupted_frame_address;
        });
    }

    rebalance(core_local);
    let next_slot_index = core_local.run_queue().lock().pop_front();

    let next = next_slot_index.and_then(|slot_index| {
        with_control_block(slot_index, |control_block| {
            control_block.state = ActorState::Running;
            let lower_half = control_block.address_space.as_ref().map(|space| {
                (
                    space.root_table_address(),
                    control_block.address_space_identifier,
                )
            });
            (control_block.saved_frame_address, lower_half)
        })
        .map(|(frame_address, lower_half)| (slot_index, frame_address, lower_half))
    });

    match next {
        Some((slot_index, frame_address, lower_half)) => {
            if current_slot_index != NO_ACTOR {
                let _ = with_control_block(current_slot_index, |control_block| {
                    control_block.state = ActorState::Runnable;
                });
                let _ = core_local.run_queue().lock().push_back(current_slot_index);
            }
            match lower_half {
                Some((root_table_address, identifier)) => {
                    activate_lower_half(root_table_address, identifier);
                }
                None => deactivate_lower_half(),
            }
            core_local.set_current_slot_index(slot_index);
            frame_address
        }
        // Nothing else to run: the current actor keeps the core.
        None if current_slot_index != NO_ACTOR => interrupted_frame_address,
        None => {
            deactivate_lower_half();
            core_local.idle_frame_address()
        }
    }
}

/// Handles the timer interrupt and returns the address of the frame to resume.
#[must_use]
pub fn on_timer_tick(interrupted_frame_address: usize) -> usize {
    rearm_tick(tick_interval());

    let Some(core_local) = current_core_local() else {
        return interrupted_frame_address;
    };

    schedule(core_local, interrupted_frame_address)
}
