//! Actor-related system calls.

use core::fmt::Write;

use foundation::{address::LocalAddress, system_call_status::SystemCallStatus};

use crate::{
    actors::{control_block::ActorState, table::with_control_block},
    architecture::aarch64::{early_console::EarlyConsole, exceptions::TrapFrame},
    scheduling::{
        core_local::{current_core_local, NO_ACTOR},
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

/// `FinishActor`: ends the calling actor and resumes someone else (or idle).
///
/// Frames and page tables are not returned to the allocator yet — that comes with full lifecycle.
#[must_use]
pub fn finish_actor(trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    let Some(core_local) = current_core_local() else {
        return trap_frame;
    };
    let slot_index = core_local.current_slot_index();
    if slot_index == NO_ACTOR {
        return trap_frame;
    }
    
    let _ = writeln!(EarlyConsole, "FinishActor slot {slot_index}");

    let _ = with_control_block(slot_index, |control_block| {
        control_block.state = ActorState::Free;
        control_block.generation_counter = control_block.generation_counter.wrapping_add(1);
        control_block.parent_address = None;
        control_block.configuration = None;
        control_block.saved_frame_address = 0;
        control_block.address_space = None;
        control_block.address_space_identifier = 0;
    });

    // Do not put this actor back on a run queue.
    core_local.set_current_slot_index(NO_ACTOR);

    let next_frame = scheduler::pick_next_after_finish(core_local);
    next_frame as *mut TrapFrame
}
