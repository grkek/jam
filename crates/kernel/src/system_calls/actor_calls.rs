//! Actor-related system calls.

use foundation::{address::LocalAddress, cause::Cause, system_call_status::SystemCallStatus};

use crate::{
    actors::{lifecycle, table::with_control_block},
    architecture::aarch64::exceptions::TrapFrame,
    scheduling::core_local::{NO_ACTOR, current_core_local},
};

/// Packs a local address into one register: generation in the high half, slot in the low half.
pub fn pack_local_address(address: LocalAddress) -> u64 {
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

/// `FinishActor`: relinquishes the current actor and resumes the next or idle frame.
///
/// This operation must never return the terminating actor's original frame.
/// A call with no currently scheduled actor indicates kernel state corruption.
#[must_use]
pub fn finish_actor(_trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    lifecycle::end_current_actor(Cause::Completed)
        .expect("FinishActor invoked without a running actor") as *mut TrapFrame
}
