//! Actor-related system calls.

use foundation::{address::LocalAddress, system_call_status::SystemCallStatus};

use crate::{
    actors::{control_block::ActorState, table::with_control_block},
    architecture::aarch64::exceptions::TrapFrame,
    scheduling::{
        core_local::{NO_ACTOR, current_core_local},
        scheduler,
    },
};

/// Packs a local address into one register: generation in the high half, slot in the low half.
fn pack_local_address(address: LocalAddress) -> u64 {
    u64::from(address.slot_index()) | (u64::from(address.generation_counter()) << 32)
}

/// `CurrentAddress`: status in `x0`, packed `LocalAddress` in `x1`.
#[must_use]
pub fn current_address() -> (SystemCallStatus, u64) {
    let Some(core_local) = current_core_local() else {
        return (SystemCallStatus::InvalidArgument, 0);
    };
    let slot_index = core_local.current_slot_index();
    if slot_index == NO_ACTOR {
        return (SystemCallStatus::InvalidArgument, 0);
    }
    let Some(generation_counter) =
        with_control_block(slot_index, |control_block| control_block.generation_counter)
    else {
        return (SystemCallStatus::InvalidArgument, 0);
    };
    let Ok(slot_number) = u32::try_from(slot_index) else {
        return (SystemCallStatus::InvalidArgument, 0);
    };
    let address = LocalAddress::new(slot_number, generation_counter);
    (SystemCallStatus::Accepted, pack_local_address(address))
}

/// `FinishActor`: ends the calling actor, reclaims its allocated memory resources,
/// and resumes execution for the next scheduled task (or idle core).
///
/// This routine safely detaches the actor's `AddressSpace`, destroys its translation
/// tables, and returns all owned frames (page tables, code pages, and stack frames)
/// back to the global physical frame allocator before context-switching away.
#[must_use]
pub fn finish_actor(trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    let Some(core_local) = current_core_local() else {
        return trap_frame;
    };
    let slot_index = core_local.current_slot_index();
    if slot_index == NO_ACTOR {
        return trap_frame;
    }

    // Take the address space out of the control block and flatten the resulting Option<Option<...>>
    let address_space = with_control_block(slot_index, |control_block| {
        control_block.state = ActorState::Free;
        control_block.generation_counter = control_block.generation_counter.wrapping_add(1);
        control_block.parent_address = None;
        control_block.configuration = None;
        control_block.saved_frame_address = 0;
        control_block.address_space_identifier = 0;
        control_block.address_space.take()
    })
    .flatten();

    // If an address space was active, destroy it and return all its frames to the pool
    if let Some(address_space) = address_space {
        let _ = crate::memory::frame_allocator::with_frame_allocator(|frame_allocator| {
            address_space.destroy(frame_allocator);
        });
    }

    // Do not put this actor back on a run queue.
    core_local.set_current_slot_index(NO_ACTOR);

    let next_frame = scheduler::pick_next_after_finish(core_local);
    next_frame as *mut TrapFrame
}
