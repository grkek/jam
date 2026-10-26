//! The scheduler: round-robin on each core, with idle cores stealing work.
//!
//! The timer interrupt saves the running context as a frame, picks the next actor, and returns
//! that actor's frame to resume. A context is only ever made runnable after its frame is saved,
//! and the handler runs on the core's own interrupt stack, so another core can resume the actor
//! the moment it is queued.

use core::fmt::Write;

use super::core_local::{CoreLocal, NO_ACTOR, core_locals, current_core_local};
use crate::{
    actors::{control_block::ActorState, table::with_control_block},
    architecture::aarch64::{
        early_console::EarlyConsole,
        generic_timer::{rearm_tick, start_tick},
        interrupt_controller::initialise_core_interface,
        processor::enable_interrupts,
        time,
    },
};

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

/// Takes work from another core's queue, newest first.
fn steal_from_other_cores(own_core_index: usize) -> Option<usize> {
    for core_local in core_locals() {
        if core_local.core_index() == own_core_index || !core_local.is_online() {
            continue;
        }

        let stolen_slot_index = core_local.run_queue().lock().pop_back();

        if stolen_slot_index.is_some() {
            return stolen_slot_index;
        }
    }
    None
}

/// Saves the interrupted context and returns the frame of the context to run next.
fn schedule(core_local: &CoreLocal, interrupted_frame_address: usize) -> usize {
    let current_slot_index = core_local.current_slot_index();
    if current_slot_index == NO_ACTOR {
        core_local.set_idle_frame_address(interrupted_frame_address);
    } else {
        let _ = with_control_block(current_slot_index, |control_block| {
            control_block.saved_frame_address = interrupted_frame_address;
            control_block.state = ActorState::Runnable;
        });
        let _ = core_local.run_queue().lock().push_back(current_slot_index);
    }

    // The own queue's lock is released before stealing, so two cores never hold each other's.
    let own_next_slot_index = core_local.run_queue().lock().pop_front();
    let next_slot_index =
        own_next_slot_index.or_else(|| steal_from_other_cores(core_local.core_index()));

    let next_frame_address = next_slot_index.and_then(|slot_index| {
        with_control_block(slot_index, |control_block| {
            control_block.state = ActorState::Running;
            control_block.saved_frame_address
        })
        .map(|frame_address| (slot_index, frame_address))
    });

    if let Some((slot_index, frame_address)) = next_frame_address {
        core_local.set_current_slot_index(slot_index);
        frame_address
    } else {
        core_local.set_current_slot_index(NO_ACTOR);
        core_local.idle_frame_address()
    }
}

/// Handles the timer interrupt and returns the address of the frame to resume.
#[must_use]
pub fn on_timer_tick(interrupted_frame_address: usize) -> usize {
    rearm_tick(tick_interval());
    let Some(core_local) = current_core_local() else {
        return interrupted_frame_address;
    };

    let tick_count = core_local.increment_tick_count();
    if core_local.core_index() == 0 && tick_count % TICKS_PER_SECOND == 0 {
        let _ = writeln!(
            EarlyConsole,
            "Heartbeat: {} seconds since the timer started",
            tick_count / TICKS_PER_SECOND
        );
    }

    schedule(core_local, interrupted_frame_address)
}
