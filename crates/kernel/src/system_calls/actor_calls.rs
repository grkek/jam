//! Actor-related system calls.

use foundation::{
    address::LocalAddress,
    cause::Cause,
    name::{MAXIMUM_NAME_BYTE_COUNT, Name},
    system_call_status::SystemCallStatus,
};

use crate::{
    actors::{
        control_block::State,
        lifecycle,
        table::{find_actor_by_name as find_local_actor_by_name, with_control_block},
    },
    architecture::aarch64::exceptions::TrapFrame,
    memory::user_copy::copy_from_actor,
    scheduling::core_local::{NO_ACTOR, current_core_local},
};

/// Packs a local address into one register: generation in the high half, slot in the low half.
#[must_use]
pub fn pack_local_address(address: LocalAddress) -> u64 {
    u64::from(address.slot_index()) | (u64::from(address.generation_counter()) << 32)
}

/// Unpacks a register value made by [`pack_local_address`]: slot in the low half,
/// generation in the high half.
#[must_use]
pub fn unpack_local_address(packed: u64) -> LocalAddress {
    let [s0, s1, s2, s3, g0, g1, g2, g3] = packed.to_le_bytes();

    LocalAddress::new(
        u32::from_le_bytes([s0, s1, s2, s3]),
        u32::from_le_bytes([g0, g1, g2, g3]),
    )
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

/// `FindActorByName`: name pointer in `x0`, length in `x1`.
///
/// Returns the status in `x0` and packed `LocalAddress` in `x1`.
/// Only live actors registered on this node are considered.
#[must_use]
pub fn find_actor_by_name(name_pointer: u64, name_byte_count: u64) -> (SystemCallStatus, u64) {
    let result = (|| {
        let byte_count =
            usize::try_from(name_byte_count).map_err(|_| SystemCallStatus::InvalidArgument)?;

        if byte_count == 0 || byte_count > MAXIMUM_NAME_BYTE_COUNT {
            return Err(SystemCallStatus::InvalidArgument);
        }

        let pointer =
            usize::try_from(name_pointer).map_err(|_| SystemCallStatus::InvalidUserMemory)?;

        let core_local = current_core_local().ok_or(SystemCallStatus::InvalidArgument)?;

        let slot_index = core_local.current_slot_index();

        if slot_index == NO_ACTOR {
            return Err(SystemCallStatus::InvalidArgument);
        }

        let mut name_bytes = [0u8; MAXIMUM_NAME_BYTE_COUNT];

        // Copy the name while the caller's address space is pinned.
        // Release the control-block lock before scanning other actors.
        with_control_block(slot_index, |control_block| {
            if control_block.state != State::Running {
                return Err(SystemCallStatus::InvalidArgument);
            }

            let address_space = control_block
                .address_space
                .as_ref()
                .ok_or(SystemCallStatus::InvalidUserMemory)?;

            copy_from_actor(address_space, pointer, &mut name_bytes[..byte_count])
        })
        .ok_or(SystemCallStatus::InvalidArgument)??;

        let name_text = core::str::from_utf8(&name_bytes[..byte_count])
            .map_err(|_| SystemCallStatus::InvalidArgument)?;

        let name = Name::new(name_text).map_err(|_| SystemCallStatus::InvalidArgument)?;

        let address = find_local_actor_by_name(name).ok_or(SystemCallStatus::ActorNotFound)?;

        Ok(pack_local_address(address))
    })();

    match result {
        Ok(address) => (SystemCallStatus::Accepted, address),
        Err(status) => (status, 0),
    }
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
