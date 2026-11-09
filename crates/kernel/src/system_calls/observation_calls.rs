//! Read-only diagnostic calls with checked user copies and bounded records.
//!
//! These snapshots are available to every local actor. They contain identities,
//! states, and aggregate resource counts, never kernel pointers or payloads.

use crate::{
    actors::{
        control_block::ActorState,
        message_storage::{ACTOR_INBOX_BYTE_BUDGET, MESSAGE_POOL, MESSAGE_POOL_CAPACITY},
        table::{ACTOR_MAXIMUM, with_control_block},
    },
    architecture::aarch64::exceptions::TrapFrame,
    memory::{frame_allocator::with_frame_allocator, user_copy::copy_to_actor},
    scheduling::core_local::{current_core_local, online_core_count},
    system_calls::actor_calls::pack_local_address,
};
use foundation::address::LocalAddress;
use foundation::{observation, system_call_status::SystemCallStatus};

/// Collects one slot under its own lock, without retaining it during copy-out.
fn actor_record(slot: usize) -> Result<observation::ActorRecord, SystemCallStatus> {
    let slot_number = u32::try_from(slot).map_err(|_| SystemCallStatus::InvalidArgument)?;
    with_control_block(slot, |actor| {
        let state = match actor.state {
            ActorState::Free => observation::ActorState::Free,
            ActorState::Runnable => observation::ActorState::Runnable,
            ActorState::Running => observation::ActorState::Running,
            ActorState::Waiting => observation::ActorState::Waiting,
            ActorState::Exiting => observation::ActorState::Exiting,
        };
        [
            observation::RECORD_VERSION,
            pack_local_address(LocalAddress::new(slot_number, actor.generation_counter)),
            state as u64,
            u64::from(actor.parent_address.is_some()),
            actor.parent_address.map_or(0, pack_local_address),
            actor
                .inbox
                .as_ref()
                .map_or(0, crate::actors::inbox::Inbox::message_count) as u64,
            actor
                .inbox
                .as_ref()
                .map_or(0, crate::actors::inbox::Inbox::resident_bytes) as u64,
            if actor.inbox.is_some() {
                ACTOR_INBOX_BYTE_BUDGET as u64
            } else {
                0
            },
        ]
    })
    .ok_or(SystemCallStatus::InvalidArgument)
}

/// Samples independently locked subsystems without nesting their locks.
fn statistics_record() -> Result<observation::StatisticsRecord, SystemCallStatus> {
    let (total, free) = with_frame_allocator(|allocator| {
        (allocator.total_frame_count(), allocator.free_frame_count())
    })
    .ok_or(SystemCallStatus::UnsupportedOperation)?;
    let pool = MESSAGE_POOL.lock().statistics();
    let mut occupied = 0_u64;
    let mut waiting = 0_u64;
    for slot in 0..ACTOR_MAXIMUM {
        let state = with_control_block(slot, |actor| actor.state)
            .ok_or(SystemCallStatus::InvalidArgument)?;
        occupied += u64::from(state != ActorState::Free);
        waiting += u64::from(state == ActorState::Waiting);
    }
    Ok([
        observation::RECORD_VERSION,
        total as u64,
        free as u64,
        MESSAGE_POOL_CAPACITY as u64,
        pool.allocated as u64,
        pool.charged_bytes as u64,
        online_core_count() as u64,
        ACTOR_MAXIMUM as u64,
        occupied,
        waiting,
    ])
}

/// Copies to the currently executing actor while its address space is pinned by its lock.
fn copy_record(pointer: u64, bytes: &[u8]) -> Result<(), SystemCallStatus> {
    let pointer = usize::try_from(pointer).map_err(|_| SystemCallStatus::InvalidUserMemory)?;
    let core = current_core_local().ok_or(SystemCallStatus::InvalidArgument)?;
    with_control_block(core.current_slot_index(), |actor| {
        if actor.state != ActorState::Running {
            return Err(SystemCallStatus::InvalidArgument);
        }
        let space = actor
            .address_space
            .as_ref()
            .ok_or(SystemCallStatus::InvalidUserMemory)?;
        copy_to_actor(space, pointer, bytes)
    })
    .ok_or(SystemCallStatus::InvalidArgument)?
}

/// Handles a fixed-size observation record on the dispatcher's exclusive frame.
pub fn read(frame: &mut TrapFrame, statistics: bool) {
    let arguments: [u64; 6] =
        core::array::from_fn(|index| frame.general_registers.get(index).copied().unwrap_or(0));
    let [first, second, third, fourth, fifth, sixth] = arguments;
    let (pointer, capacity, required) = if statistics {
        (first, second, observation::STATISTICS_RECORD_BYTE_COUNT)
    } else {
        (second, third, observation::ACTOR_RECORD_BYTE_COUNT)
    };
    let mut value = 0;
    let result = (|| {
        if fourth != 0 || fifth != 0 || sixth != 0 || (statistics && third != 0) {
            return Err(SystemCallStatus::InvalidArgument);
        }
        if !statistics && first >= ACTOR_MAXIMUM as u64 {
            return Err(SystemCallStatus::InvalidArgument);
        }
        if capacity < required as u64 {
            value = required as u64;
            return Err(SystemCallStatus::BufferTooSmall);
        }
        let mut bytes = [0_u8; observation::STATISTICS_RECORD_BYTE_COUNT];
        let output = bytes
            .get_mut(..required)
            .ok_or(SystemCallStatus::InvalidArgument)?;
        let encoded = if statistics {
            observation::encode(&statistics_record()?, output)
        } else {
            observation::encode(
                &actor_record(
                    usize::try_from(first).map_err(|_| SystemCallStatus::InvalidArgument)?,
                )?,
                output,
            )
        };
        if !encoded {
            return Err(SystemCallStatus::InvalidArgument);
        }
        copy_record(pointer, output)?;
        value = if statistics {
            required as u64
        } else {
            ACTOR_MAXIMUM as u64
        };
        Ok(())
    })();
    if let Some(register) = frame.general_registers.get_mut(0) {
        *register = result.err().unwrap_or(SystemCallStatus::Accepted).number();
    }
    if let Some(register) = frame.general_registers.get_mut(1) {
        *register = value;
    }
}
