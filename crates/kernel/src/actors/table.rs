//! The table of every actor on this node.

use foundation::{address::LocalAddress, configuration::Configuration};

use super::control_block::{ActorState, ControlBlock};
use crate::{
    memory::kernel_stack::prepare_first_frame, scheduling::scheduler::enqueue_runnable,
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
}

/// Starts an actor that runs in kernel mode, beginning at `entry_point`.
///
/// # Errors
///
/// Fails when the table is full.
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

        control_block.state = ActorState::Runnable;
        control_block.parent_address = parent_address;
        control_block.configuration = Some(configuration);
        control_block.saved_frame_address = frame_address;

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
