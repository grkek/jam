//! Read-only actor census and node statistics system calls.
//!
//! Census reads return bounded batches of fixed-size records. Each actor record
//! is sampled under its own control-block lock, which is released before the
//! record is copied to EL0. Separate records are not an atomic table snapshot.
//!
//! Statistics read independently maintained subsystem counters without scanning
//! the actor table. All user-memory writes use checked copies, and neither
//! operation exposes kernel pointers or message payloads.

use foundation::{
    address::LocalAddress,
    observation,
    system_call::{STATUS_REGISTER, VALUE_REGISTER},
    system_call_status::SystemCallStatus,
};

use crate::{
    actors::{
        control_block::State,
        message_storage::{ACTOR_INBOX_BYTE_BUDGET, MESSAGE_POOL, MESSAGE_POOL_CAPACITY},
        table::{actor_table_length, with_control_block},
    },
    architecture::aarch64::exceptions::TrapFrame,
    memory::{frame_allocator::with_frame_allocator, user_copy::copy_to_actor},
    scheduling::core_local::{current_core_local, online_core_count},
    system_calls::actor_calls::pack_local_address,
};

/// Samples one actor slot without retaining its lock during user copy-out.
///
/// Slots inside the installed table that have no backing control block are
/// represented as free. Out-of-range slots are rejected by the syscall handler.
fn actor_record(slot_index: usize) -> Result<observation::ActorRecord, SystemCallStatus> {
    let slot_number = u32::try_from(slot_index).map_err(|_| SystemCallStatus::InvalidArgument)?;

    let record = with_control_block(slot_index, |actor| {
        let state = match actor.state {
            State::Free => observation::State::Free,
            State::Runnable => observation::State::Runnable,
            State::Running => observation::State::Running,
            State::Waiting => observation::State::Waiting,
            State::Exiting => observation::State::Exiting,
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
    });

    Ok(record.unwrap_or([
        observation::RECORD_VERSION,
        pack_local_address(LocalAddress::new(slot_number, 0)),
        observation::State::Free as u64,
        0,
        0,
        0,
        0,
        0,
    ]))
}

/// Samples independently maintained counters without scanning actor slots.
fn statistics_record() -> Result<observation::StatisticsRecord, SystemCallStatus> {
    let (total_frames, free_frames) = with_frame_allocator(|allocator| {
        (allocator.total_frame_count(), allocator.free_frame_count())
    })
    .ok_or(SystemCallStatus::UnsupportedOperation)?;

    let message_pool = MESSAGE_POOL.lock().statistics();
    let (occupied_actors, waiting_actors) = crate::actors::snapshot::current();

    Ok([
        observation::RECORD_VERSION,
        total_frames as u64,
        free_frames as u64,
        MESSAGE_POOL_CAPACITY as u64,
        message_pool.allocated as u64,
        message_pool.charged_bytes as u64,
        online_core_count() as u64,
        actor_table_length() as u64,
        occupied_actors as u64,
        waiting_actors as u64,
    ])
}

/// Copies validated bytes to the currently running actor's address space.
///
/// The actor's control-block lock pins the address space for the entire copy.
fn copy_record(user_pointer: u64, bytes: &[u8]) -> Result<(), SystemCallStatus> {
    let user_pointer =
        usize::try_from(user_pointer).map_err(|_| SystemCallStatus::InvalidUserMemory)?;
    let core_local = current_core_local().ok_or(SystemCallStatus::InvalidArgument)?;

    with_control_block(core_local.current_slot_index(), |actor| {
        if actor.state != State::Running {
            return Err(SystemCallStatus::InvalidArgument);
        }

        let address_space = actor
            .address_space
            .as_ref()
            .ok_or(SystemCallStatus::InvalidUserMemory)?;

        copy_to_actor(address_space, user_pointer, bytes)
    })
    .ok_or(SystemCallStatus::InvalidArgument)?
}

/// Handles `ReadStatistics` with one fixed-size, versioned record.
fn read_statistics(arguments: [u64; 6], return_value: &mut u64) -> Result<(), SystemCallStatus> {
    let [user_pointer, byte_capacity, reserved_two, reserved_three, reserved_four, reserved_five] =
        arguments;

    if reserved_two != 0 || reserved_three != 0 || reserved_four != 0 || reserved_five != 0 {
        return Err(SystemCallStatus::InvalidArgument);
    }

    let required_byte_count = observation::STATISTICS_RECORD_BYTE_COUNT;
    if byte_capacity < required_byte_count as u64 {
        *return_value = required_byte_count as u64;
        return Err(SystemCallStatus::BufferTooSmall);
    }

    let record = statistics_record()?;
    let mut bytes = [0_u8; observation::STATISTICS_RECORD_BYTE_COUNT];
    if !observation::encode(&record, &mut bytes) {
        return Err(SystemCallStatus::InvalidArgument);
    }

    copy_record(user_pointer, &bytes)?;
    *return_value = required_byte_count as u64;
    Ok(())
}

/// Handles `ReadCensus` with at most one bounded batch of actor records.
///
/// `x1` continues to report the installed actor-table length on success. A
/// caller providing a single-record buffer retains the original behavior.
fn read_census(arguments: [u64; 6], return_value: &mut u64) -> Result<(), SystemCallStatus> {
    let [
        first_slot_number,
        user_pointer,
        byte_capacity,
        reserved_three,
        reserved_four,
        reserved_five,
    ] = arguments;

    if reserved_three != 0 || reserved_four != 0 || reserved_five != 0 {
        return Err(SystemCallStatus::InvalidArgument);
    }

    let first_slot =
        usize::try_from(first_slot_number).map_err(|_| SystemCallStatus::InvalidArgument)?;
    let actor_capacity: usize = actor_table_length();
    if first_slot >= actor_capacity {
        return Err(SystemCallStatus::InvalidArgument);
    }

    let record_byte_count = observation::ACTOR_RECORD_BYTE_COUNT;
    if byte_capacity < record_byte_count as u64 {
        *return_value = record_byte_count as u64;
        return Err(SystemCallStatus::BufferTooSmall);
    }

    let available_record_count =
        usize::try_from(byte_capacity / record_byte_count as u64).unwrap_or(usize::MAX);
    let record_count = available_record_count
        .min(observation::MAXIMUM_CENSUS_RECORDS_PER_CALL)
        .min(actor_capacity - first_slot);

    let mut bytes = [0_u8; observation::CENSUS_BATCH_BYTE_COUNT];
    for record_index in 0..record_count {
        let record_start = record_index * record_byte_count;
        let record_end = record_start + record_byte_count;
        let output = bytes
            .get_mut(record_start..record_end)
            .ok_or(SystemCallStatus::InvalidArgument)?;
        let record = actor_record(first_slot + record_index)?;

        if !observation::encode(&record, output) {
            return Err(SystemCallStatus::InvalidArgument);
        }
    }

    let copied_byte_count = record_count * record_byte_count;
    copy_record(user_pointer, &bytes[..copied_byte_count])?;
    *return_value = actor_capacity as u64;
    Ok(())
}

/// Dispatches one observation call and writes its status and value to the frame.
///
/// `ReadCensus` returns the actor-table length in `x1`; `ReadStatistics` returns
/// the encoded record length. A short output buffer returns `BufferTooSmall`
/// with the minimum record byte count in `x1`.
pub fn read(frame: &mut TrapFrame, statistics: bool) {
    let arguments: [u64; 6] = core::array::from_fn(|index| frame.general_registers[index]);
    let mut return_value = 0_u64;

    let result = if statistics {
        read_statistics(arguments, &mut return_value)
    } else {
        read_census(arguments, &mut return_value)
    };

    frame.general_registers[usize::from(STATUS_REGISTER)] =
        result.err().unwrap_or(SystemCallStatus::Accepted).number();
    frame.general_registers[usize::from(VALUE_REGISTER)] = return_value;
}
