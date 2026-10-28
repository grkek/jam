//! How an actor's life ends.

use core::fmt::Write;

use foundation::{address::LocalAddress, cause::Cause};

use super::{control_block::ActorState, table::with_control_block};
use crate::{
    architecture::aarch64::{
        early_console::EarlyConsole,
        memory_management_unit::{deactivate_lower_half, invalidate_address_space_identifier},
    },
    memory::frame_allocator::with_frame_allocator,
    scheduling::{
        core_local::{NO_ACTOR, current_core_local},
        scheduler::pick_next_after_finish,
    },
};

/// Ends the actor running on this core for `cause`, takes back everything it owned, and returns
/// the frame this core resumes next. Returns `None` when no actor runs here.
///
/// The slot passes through `Exiting` and only becomes `Free`, with a new generation, once its
/// memory is back, so no other core can spawn into it half torn down.
pub fn end_current_actor(cause: Cause) -> Option<usize> {
    let core_local = current_core_local()?;
    let slot_index = core_local.current_slot_index();
    if slot_index == NO_ACTOR {
        return None;
    }

    let (address, address_space, address_space_identifier) =
        with_control_block(slot_index, |control_block| {
            control_block.state = ActorState::Exiting;
            control_block.exit_cause = Some(cause);
            let slot_number = u32::try_from(slot_index).unwrap_or(u32::MAX);
            (
                LocalAddress::new(slot_number, control_block.generation_counter),
                control_block.address_space.take(),
                control_block.address_space_identifier,
            )
        })?;

    let _ = writeln!(EarlyConsole, "Actor {address} ended: {cause:?}");

    // Stop walking the dead actor's tables and drop its cached translations everywhere, then
    // give its frames back.
    deactivate_lower_half();
    invalidate_address_space_identifier(address_space_identifier);

    if let Some(address_space) = address_space {
        let _ = with_frame_allocator(|frame_allocator| address_space.destroy(frame_allocator));
    }

    core_local.set_current_slot_index(NO_ACTOR);

    let _ = with_control_block(slot_index, |control_block| {
        control_block.state = ActorState::Free;
        control_block.generation_counter = control_block.generation_counter.wrapping_add(1);
        control_block.parent_address = None;
        control_block.configuration = None;
        control_block.saved_frame_address = 0;
        control_block.address_space_identifier = 0;
    });

    Some(pick_next_after_finish(core_local))
}
