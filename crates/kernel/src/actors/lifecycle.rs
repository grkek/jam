//! Actor termination, message cleanup, and safe actor-slot recycling.
//!
//! A finishing actor first becomes `Exiting`, which closes message admission.
//! Its kernel-owned messages and retained references are reclaimed, then its
//! address space is destroyed. The table slot is published as `Free` only when
//! the actor is no longer current on any core and the cleanup has completed.
//!
//! An unsuccessful cleanup leaves the actor quarantined in `Exiting` rather
//! than allowing unsafe slot reuse. This module currently handles self-exit;
//! forcibly terminating actors running on other cores requires an additional
//! scheduler stop/quiescence protocol.

use core::fmt::Write;

use foundation::{address::LocalAddress, cause::Cause};

use super::{
    control_block::ActorState,
    message_cleanup::{begin_actor_message_exit, finish_actor_message_cleanup},
    message_storage::MESSAGE_POOL,
    table::with_control_block,
};
use crate::{
    architecture::aarch64::{
        early_console::EarlyConsole,
        memory_management_unit::{deactivate_lower_half, invalidate_address_space_identifier},
        processor::interrupts_enabled
    },
    information,
    error,
    memory::frame_allocator::with_frame_allocator,
    scheduling::{
        core_local::{NO_ACTOR, core_locals, current_core_local},
        scheduler::pick_next_after_finish,
    },
};

/// Reclaims the finishing actor's messages and, if safe, its address space and slot.
///
/// The caller must have stopped running user code and relinquished the core's
/// current actor before calling this function. On failure, resources remain
/// quarantined in `Exiting` and must be handled by a future reaper.
fn reclaim_exiting_actor(address: LocalAddress) {
    let complete = match finish_actor_message_cleanup(address, &MESSAGE_POOL, |released| {
        // MAX_REFERENCES is currently zero; after Scroll integration this
        // callback must release every retained capability explicitly.
        drop(released);
    }) {
        Ok(complete) => complete,
        Err(error) => {
            let _ = writeln!(EarlyConsole, "Actor {address} cleanup failed: {error:?}");
            false
        }
    };
    if !complete {
        return;
    }

    let slot_index = address.slot_index() as usize;
    if core_locals()
        .iter()
        .any(|core| core.current_slot_index() == slot_index)
    {
        // No other core is allowed to run this incarnation. Fail closed if
        // an invariant is violated; do not destroy potentially active memory.
        return;
    }

    let detached = with_control_block(slot_index, |actor| {
        if actor.state != ActorState::Exiting
            || actor.generation_counter != address.generation_counter()
            || actor.inbox.is_some()
            || actor.pending_message_releases != 0
            || actor.queued
        {
            return None;
        }
        Some((actor.address_space.take(), actor.address_space_identifier))
    })
    .flatten();
    let Some((address_space, address_space_identifier)) = detached else {
        return;
    };

    // The dying actor has already been descheduled and TTBR0 deactivated.
    // Invalidating its ASID before frame reuse prevents stale cached mappings.
    invalidate_address_space_identifier(address_space_identifier);

    if let Some(address_space) = address_space {
        // Keep ownership in an Option so an unavailable frame allocator
        // does not accidentally drop the actor's address-space descriptor.
        let mut outstanding_address_space = Some(address_space);
        let released = with_frame_allocator(|allocator| {
            if let Some(address_space) = outstanding_address_space.take() {
                let _ = address_space.destroy(allocator);
            }
        });
        if released.is_none() {
            let _ = with_control_block(slot_index, |actor| {
                if actor.state == ActorState::Exiting
                    && actor.generation_counter == address.generation_counter()
                    && actor.address_space.is_none()
                {
                    actor.address_space = outstanding_address_space.take();
                }
            });
            let _ = information!(
                EarlyConsole,
                "Actor {address}: allocator unavailable during exit"
            );
            return;
        }
    }

    let _ = with_control_block(slot_index, |actor| {
        if actor.state != ActorState::Exiting
            || actor.generation_counter != address.generation_counter()
            || actor.inbox.is_some()
            || actor.pending_message_releases != 0
            || actor.queued
        {
            return;
        }

        // No wraparound: an exhausted slot is permanently retired rather
        // than permitting a stale LocalAddress to become valid again.
        let Some(next_generation) = actor.generation_counter.checked_add(1) else {
            // The exhausted actor remains in `Exiting` permanently.
            return;
        };
        actor.generation_counter = next_generation;
        actor.parent_address = None;
        actor.configuration = None;
        actor.saved_frame_address = 0;
        actor.address_space_identifier = 0;
        actor.exit_cause = None;
        actor.state = ActorState::Free;

        information!(EarlyConsole, "Actor {address} slot freed, next generation {next_generation}");
    });
}

/// Ends the actor running on this core and returns the next runnable or idle frame.
///
/// The actor leaves `Running` permanently, closes its inbox, and releases
/// all queued messages before its address space and table slot are reclaimed.
/// An incomplete cleanup retains `Exiting` and cannot result in slot reuse.
///
/// Returns `None` only if the current core has no running actor. The caller
/// must never resume the original actor after a successful termination.
#[must_use]
pub fn end_current_actor(cause: Cause) -> Option<usize> {
    let core_local = current_core_local()?;
    let core_index = core_local.core_index();
    let slot_index = core_local.current_slot_index();
    if slot_index == NO_ACTOR {
        error!(EarlyConsole, "end_current_actor: core {core_index} has no current slot");
        return None;
    }

    let Ok(slot_number) = u32::try_from(slot_index) else {
        error!(EarlyConsole, "end_current_actor: core {core_index} slot {slot_index} does not fit u32");
        return None;
    };

    let snapshot = with_control_block(slot_index, |actor| {
        (actor.state, actor.generation_counter, actor.queued)
    });
    let Some((state, generation, queued)) = snapshot else {
        error!(EarlyConsole, "end_current_actor: core {core_index} invalid slot {slot_index}");
        return None;
    };
    if state != ActorState::Running {
        error!(
            EarlyConsole,
            "end_current_actor: core {core_index} slot {slot_index} gen {generation} state {state:?} queued {queued} (expected Running)"
        );
        return None;
    }
    let address = LocalAddress::new(slot_number, generation);

    assert!(
        !interrupts_enabled(),
        "end_current_actor requires IRQs masked"
    );
    if let Err(error) = begin_actor_message_exit(address) {
        panic!("end_current_actor: core {core_index} {address} begin exit failed: {error:?}");
    }

    let _ = with_control_block(slot_index, |actor| {
        if actor.state == ActorState::Exiting
            && actor.generation_counter == address.generation_counter()
        {
            actor.exit_cause = Some(cause);
        }
    });

    information!(EarlyConsole, "Actor {address} ended on core {core_index}: {cause:?}");

    deactivate_lower_half();
    core_local.set_current_slot_index(NO_ACTOR);

    reclaim_exiting_actor(address);
    let next = pick_next_after_finish(core_local);
    information!(EarlyConsole, "core {core_index} after {address}: next frame {next:#x}");
    Some(next)
}