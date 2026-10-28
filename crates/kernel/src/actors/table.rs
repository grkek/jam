//! The table of every actor on this node.

use foundation::{address::LocalAddress, configuration::Configuration};

use super::control_block::{ActorState, ControlBlock};
use crate::{
    architecture::aarch64::memory_management_unit::invalidate_address_space_identifier,
    memory::{
        frame_allocator::with_frame_allocator,
        kernel_stack::{prepare_first_frame, prepare_first_user_frame},
        private_page::{PRIVATE_VALUE_STRIDE, build_address_space_with_private_page},
        user_memory::UserMapError,
        user_program::{build_elf_user_address_space, elf_user_stack_top},
    },
    scheduling::scheduler::enqueue_runnable,
    synchronization::spin_lock::SpinLock,
};

/// Maximum number of actors that can exist simultaneously on this node.
pub const ACTOR_MAXIMUM: usize = 64;

static ACTOR_TABLE: [SpinLock<ControlBlock>; ACTOR_MAXIMUM] =
    [const { SpinLock::new(ControlBlock::EMPTY) }; ACTOR_MAXIMUM];

/// Why an actor could not be spawned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// Every actor-table slot is occupied.
    TableFull,
    /// The actor's initial kernel stack frame could not be prepared.
    StackUnavailable,
    /// The physical frame allocator is unavailable or has insufficient frames.
    OutOfMemory,
    /// The ELF executable is invalid, unsupported, or has an invalid mapping.
    InvalidExecutable,
}

/// Starts an actor at `entry_point` in kernel mode, in its own address space.
///
/// # Errors
///
/// Returns [`SpawnError`] when no actor-table slot, initial kernel stack,
/// or address-space memory is available.
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
        control_block.exit_cause = None;
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

/// Starts an EL0 actor from a statically linked AArch64 ELF64 executable.
///
/// Maps the executable's loadable segments and user stack, then prepares the
/// first user frame with the ELF entry point as `ELR_EL1` and the ELF stack top
/// as the initial `SP_EL0`.
///
/// The `program` argument must contain the complete ELF image, not raw
/// instruction bytes. Raw-code self-tests should use a separate spawn path.
///
/// # Errors
///
/// Returns [`SpawnError::InvalidExecutable`] when the ELF is malformed or has
/// an unsupported mapping, [`SpawnError::OutOfMemory`] when frame allocation
/// fails, or the corresponding error when the table/stack is unavailable.
pub fn spawn_user_actor(
    program: &'static [u8],
    parent_address: Option<LocalAddress>,
    configuration: Configuration,
) -> Result<LocalAddress, SpawnError> {
    for (slot_index, slot) in ACTOR_TABLE.iter().enumerate() {
        let mut control_block = slot.lock();
        if control_block.state != ActorState::Free {
            continue;
        }

        let address_space_identifier =
            u16::try_from(slot_index + 1).map_err(|_| SpawnError::TableFull)?;

        // The allocator closure returns Result<(AddressSpace, u64), UserMapError>.
        // Keep the absence of the allocator separate from an ELF mapping error.
        let (address_space, entry_point) = with_frame_allocator(|frame_allocator| {
            build_elf_user_address_space(
                frame_allocator,
                slot_index as u64 * PRIVATE_VALUE_STRIDE,
                program,
            )
        })
        .ok_or(SpawnError::OutOfMemory)?
        .map_err(|error| match error {
            UserMapError::OutOfFrames => SpawnError::OutOfMemory,
            _ => SpawnError::InvalidExecutable,
        })?;

        let entry_point =
            usize::try_from(entry_point).map_err(|_| SpawnError::InvalidExecutable)?;
        let frame_address = prepare_first_user_frame(slot_index, entry_point, elf_user_stack_top())
            .ok_or(SpawnError::StackUnavailable)?;

        invalidate_address_space_identifier(address_space_identifier);

        control_block.state = ActorState::Runnable;
        control_block.exit_cause = None;
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

/// Runs `operation` on the control block at `slot_index`, holding its lock.
pub fn with_control_block<Output>(
    slot_index: usize,
    operation: impl FnOnce(&mut ControlBlock) -> Output,
) -> Option<Output> {
    let slot = ACTOR_TABLE.get(slot_index)?;
    let mut control_block = slot.lock();
    Some(operation(&mut control_block))
}
