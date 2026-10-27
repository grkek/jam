//! The table of every actor on this node.

use foundation::{address::LocalAddress, configuration::Configuration};

use super::control_block::{ActorState, ControlBlock};
use crate::{
    architecture::aarch64::memory_management_unit::invalidate_address_space_identifier,
    memory::{
        frame_allocator::with_frame_allocator,
        kernel_stack::{prepare_first_frame, prepare_first_user_frame},
        private_page::{PRIVATE_VALUE_STRIDE, build_address_space_with_private_page},
        user_memory::TEST_CODE_ADDRESS,
        user_program::{build_user_address_space, user_stack_top},
    },
    scheduling::scheduler::enqueue_runnable,
    synchronization::spin_lock::SpinLock,
};

/// Most actors that can exist at once.
pub const ACTOR_MAXIMUM: usize = 64;

static ACTOR_TABLE: [SpinLock<ControlBlock>; ACTOR_MAXIMUM] =
    [const { SpinLock::new(ControlBlock::EMPTY) }; ACTOR_MAXIMUM];

/// Why an actor could not be spawned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// Every slot is taken.
    TableFull,
    /// The slot has no kernel stack.
    StackUnavailable,
    /// No memory was left for the actor's address space.
    OutOfMemory,
}

/// Starts an actor that runs in kernel mode, beginning at `entry_point`, in its own address space.
///
/// # Errors
///
/// Fails when the table is full or memory runs out.
pub fn spawn_kernel_actor(
    entry_point: extern "C" fn() -> !,
    parent_address: Option<LocalAddress>,
    configuration: Configuration,
) -> Result<LocalAddress, SpawnError> {
    for (slot_index, slot) in ACTOR_TABLE.iter().enumerate() {
        let mut control_block = slot.lock();
        if control_block.state != ActorState::Free {
            continue;
        }
        let frame_address = prepare_first_frame(slot_index, entry_point as usize)
            .ok_or(SpawnError::StackUnavailable)?;
        let address_space_identifier =
            u16::try_from(slot_index + 1).map_err(|_| SpawnError::TableFull)?;
        let address_space = with_frame_allocator(|frame_allocator| {
            build_address_space_with_private_page(
                frame_allocator,
                slot_index as u64 * PRIVATE_VALUE_STRIDE,
            )
        })
        .flatten()
        .ok_or(SpawnError::OutOfMemory)?;
        invalidate_address_space_identifier(address_space_identifier);

        control_block.state = ActorState::Runnable;
        control_block.parent_address = parent_address;
        control_block.configuration = Some(configuration);
        control_block.saved_frame_address = frame_address;
        control_block.address_space = Some(address_space);
        control_block.address_space_identifier = address_space_identifier;
        let generation_counter = control_block.generation_counter;
        drop(control_block);

        enqueue_runnable(slot_index);
        let slot_number = u32::try_from(slot_index).map_err(|_| SpawnError::TableFull)?;
        return Ok(LocalAddress::new(slot_number, generation_counter));
    }
    Err(SpawnError::TableFull)
}

/// Starts an actor that runs at EL0, beginning at the embedded `svc` loop.
///
/// # Errors
///
/// Fails when the table is full or memory runs out.
pub fn spawn_user_actor(
    parent_address: Option<LocalAddress>,
    configuration: Configuration,
) -> Result<LocalAddress, SpawnError> {
    for (slot_index, slot) in ACTOR_TABLE.iter().enumerate() {
        let mut control_block = slot.lock();
        if control_block.state != ActorState::Free {
            continue;
        }

        let frame_address =
            prepare_first_user_frame(slot_index, TEST_CODE_ADDRESS, user_stack_top())
                .ok_or(SpawnError::StackUnavailable)?;

        let address_space_identifier =
            u16::try_from(slot_index + 1).map_err(|_| SpawnError::TableFull)?;

        let address_space = with_frame_allocator(|frame_allocator| {
            build_user_address_space(frame_allocator, slot_index as u64 * PRIVATE_VALUE_STRIDE)
        })
        .transpose()
        .map_err(|_| SpawnError::OutOfMemory)?
        .ok_or(SpawnError::OutOfMemory)?;

        invalidate_address_space_identifier(address_space_identifier);

        control_block.state = ActorState::Runnable;
        control_block.parent_address = parent_address;
        control_block.configuration = Some(configuration);
        control_block.saved_frame_address = frame_address;
        control_block.address_space = Some(address_space);
        control_block.address_space_identifier = address_space_identifier;
        let generation_counter = control_block.generation_counter;
        drop(control_block);

        enqueue_runnable(slot_index);
        let slot_number = u32::try_from(slot_index).map_err(|_| SpawnError::TableFull)?;
        return Ok(LocalAddress::new(slot_number, generation_counter));
    }
    Err(SpawnError::TableFull)
}

/// Runs `operation` on the control block in `slot_index`, holding its lock.
pub fn with_control_block<Output>(
    slot_index: usize,
    operation: impl FnOnce(&mut ControlBlock) -> Output,
) -> Option<Output> {
    let slot = ACTOR_TABLE.get(slot_index)?;
    let mut control_block = slot.lock();
    Some(operation(&mut control_block))
}
