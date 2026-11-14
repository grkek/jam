//! The table of every actor on this node.
//!
//! The table is built at boot from physical frames, so its size follows the machine's memory.
//! Each chunk is one frame of control blocks; a paged directory points at the chunks.
//!
//! Actors are started with [`enqueue`], which takes a description of the actor, picks the slot
//! and returns the actor's address.

use core::sync::atomic::{AtomicUsize, Ordering};

use foundation::{
    actor::{Actor, Image},
    address::LocalAddress,
    configuration::Configuration,
    name::Name,
};

use super::control_block::{ControlBlock, State};
use crate::{
    actors::{divider::system_limit, inbox::Inbox},
    architecture::aarch64::{
        early_console::EarlyConsole, memory_management_unit::invalidate_address_space_identifier,
    },
    error,
    memory::{
        address_space::AddressSpace,
        address_translation::physical_to_virtual,
        frame_allocator::{FrameAllocator, with_frame_allocator},
        kernel_stack::{prepare_first_frame, prepare_first_user_frame},
        paged_directory::{DirectoryError, PagedDirectory},
        private_page::{PRIVATE_VALUE_STRIDE, build_address_space_with_private_page},
        user_memory::UserMapError,
        user_program::{build_elf_user_address_space, elf_user_stack_top},
    },
    scheduling::scheduler::enqueue_runnable,
    synchronization::spin_lock::SpinLock,
};

/// One slot of the actor table.
type Slot = SpinLock<ControlBlock>;

const FRAME_BYTE_COUNT: usize = 4096;

/// Bytes one slot occupies. The system limits count this against memory.
pub const ACTOR_SLOT_BYTE_COUNT: usize = size_of::<Slot>();

/// A chunk holds every slot that fits in one frame, or a single slot spanning several frames.
const SLOTS_PER_CHUNK: usize = if ACTOR_SLOT_BYTE_COUNT <= FRAME_BYTE_COUNT {
    FRAME_BYTE_COUNT / ACTOR_SLOT_BYTE_COUNT
} else {
    1
};

/// Physically adjacent frames behind one chunk.
const FRAMES_PER_CHUNK: usize =
    (SLOTS_PER_CHUNK * ACTOR_SLOT_BYTE_COUNT).div_ceil(FRAME_BYTE_COUNT);

const _: () = assert!(
    align_of::<Slot>() <= FRAME_BYTE_COUNT,
    "a control block must not need more than frame alignment"
);

/// Base address of every chunk. The chunks themselves come from the frame allocator.
static CHUNK_DIRECTORY: PagedDirectory = PagedDirectory::new();

/// Number of slots that exist. Zero until the table is fully built.
static SLOT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Why the actor table could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableError {
    /// The table was already built.
    AlreadyInitialised,
    /// No slots were requested.
    Empty,
    /// The chunk directory could not be built.
    Directory(DirectoryError),
    /// The physical frame allocator ran out of frames.
    OutOfFrames,
    /// The frame allocator did not hand out adjacent frames for a chunk.
    FramesNotContiguous,
}

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
    /// The actor's resource budget exceeds the system ceiling.
    BudgetExceedsCeiling,
    /// The scheduler refused the new actor. The slot was returned to the free state.
    SchedulerRejected,
}

/// Takes the frames of one chunk and returns the address of the first.
///
/// Boot runs before the allocator is fragmented, so consecutive allocations are expected to
/// be adjacent. The check turns a violated assumption into a boot error instead of corruption.
fn allocate_chunk_frames(frame_allocator: &mut FrameAllocator) -> Result<usize, TableError> {
    let first_frame = frame_allocator
        .allocate_frame()
        .ok_or(TableError::OutOfFrames)?;

    for frame_offset in 1..FRAMES_PER_CHUNK {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(TableError::OutOfFrames)?;
        if frame != first_frame + frame_offset * FRAME_BYTE_COUNT {
            return Err(TableError::FramesNotContiguous);
        }
    }

    Ok(first_frame)
}

/// Builds the actor table with `slot_count` free slots and returns how many frames it took.
///
/// Call once at boot, while the caller still owns the frame allocator and before the first spawn.
/// Frames already taken are not returned on failure, because boot treats that as fatal.
///
/// # Errors
///
/// Returns [`TableError`] when the table already exists, when `slot_count` is zero,
/// when the directory cannot be built, or when the frame allocator cannot supply the chunks.
pub fn initialise_actor_table(
    frame_allocator: &mut FrameAllocator,
    slot_count: usize,
) -> Result<usize, TableError> {
    if SLOT_COUNT.load(Ordering::Acquire) != 0 {
        return Err(TableError::AlreadyInitialised);
    }
    if slot_count == 0 {
        return Err(TableError::Empty);
    }

    let chunk_count = slot_count.div_ceil(SLOTS_PER_CHUNK);
    let directory_frame_count = CHUNK_DIRECTORY
        .initialise(frame_allocator, chunk_count)
        .map_err(TableError::Directory)?;

    for chunk_index in 0..chunk_count {
        let frame_address = allocate_chunk_frames(frame_allocator)?;
        let chunk = physical_to_virtual(frame_address) as *mut Slot;

        // The last chunk may be partly unused; the unused slots are never reached.
        for offset in 0..SLOTS_PER_CHUNK {
            // SAFETY: The frames are exclusively owned here, adjacent, large enough for
            // SLOTS_PER_CHUNK slots, and aligned for a slot (checked above).
            unsafe { chunk.add(offset).write(SpinLock::new(ControlBlock::EMPTY)) };
        }

        CHUNK_DIRECTORY.set(chunk_index, chunk as usize);
    }

    // Publish last, so anyone who sees a nonzero count also sees every chunk.
    SLOT_COUNT.store(slot_count, Ordering::Release);

    Ok(directory_frame_count + chunk_count * FRAMES_PER_CHUNK)
}

/// Number of slots in the actor table. Zero before [`initialise_actor_table`] has finished.
#[must_use]
pub fn actor_table_length() -> usize {
    SLOT_COUNT.load(Ordering::Acquire)
}

/// The slot at `slot_index`, or `None` when it lies outside the table.
fn actor_slot(slot_index: usize) -> Option<&'static Slot> {
    if slot_index >= actor_table_length() {
        return None;
    }

    let chunk = CHUNK_DIRECTORY.get(slot_index / SLOTS_PER_CHUNK)? as *const Slot;

    // SAFETY: The index is below the published slot count, so this chunk was fully
    // initialised before publication. Chunks are never freed or moved.
    Some(unsafe { &*chunk.add(slot_index % SLOTS_PER_CHUNK) })
}

/// Rejects a configuration whose budget is above the ceiling of this boot.
fn check_budget(configuration: &Configuration) -> Result<(), SpawnError> {
    if configuration
        .resource_budget
        .fits_within(system_limit().budget)
    {
        Ok(())
    } else {
        Err(SpawnError::BudgetExceedsCeiling)
    }
}

/// The inbox byte budget the actor was configured with.
fn inbox_byte_budget(configuration: &Configuration) -> usize {
    usize::try_from(configuration.resource_budget.inbox_byte_budget).unwrap_or(usize::MAX)
}

/// Finds the live actor registered under `name`.
#[must_use]
pub fn find_actor_by_name(name: Name) -> Option<LocalAddress> {
    if name == Name::UNNAMED {
        return None;
    }
    for slot_index in 0..actor_table_length() {
        let found = with_control_block(slot_index, |control_block| {
            if matches!(control_block.state, State::Free | State::Exiting)
                || control_block.name != name
            {
                return None;
            }
            Some(control_block.generation_counter)
        })
        .flatten();

        if let Some(generation_counter) = found {
            let slot_number = u32::try_from(slot_index).ok()?;
            return Some(LocalAddress::new(slot_number, generation_counter));
        }
    }
    None
}

/// An address space plus what is needed to build the actor's first frame.
struct PreparedImage {
    address_space: AddressSpace,
    entry_point: usize,
    /// `Some` for an EL0 actor, `None` for a kernel actor.
    user_stack_top: Option<usize>,
}

/// Turns a loader error into the error reported to the spawner.
fn map_error(error: UserMapError) -> SpawnError {
    match error {
        UserMapError::OutOfFrames => SpawnError::OutOfMemory,
        _ => SpawnError::InvalidExecutable,
    }
}

/// Builds the address space for `image`.
///
/// The private value depends on the slot, so this runs after a slot has been found.
fn build_image(image: &Image<'_>, private_value: u64) -> Result<PreparedImage, SpawnError> {
    match *image {
        Image::Elf(program) => {
            let (address_space, entry_point) = with_frame_allocator(|frames| {
                build_elf_user_address_space(frames, private_value, program)
            })
            .ok_or(SpawnError::OutOfMemory)?
            .map_err(map_error)?;
            Ok(PreparedImage {
                address_space,
                entry_point: usize::try_from(entry_point)
                    .map_err(|_| SpawnError::InvalidExecutable)?,
                user_stack_top: Some(elf_user_stack_top()),
            })
        }
        Image::Kernel(entry_point) => {
            let address_space = with_frame_allocator(|frames| {
                build_address_space_with_private_page(frames, private_value)
            })
            .flatten()
            .ok_or(SpawnError::OutOfMemory)?;
            Ok(PreparedImage {
                address_space,
                entry_point: entry_point as usize,
                user_stack_top: None,
            })
        }
    }
}

/// Gives an unused address space back to the frame allocator.
fn discard(address_space: AddressSpace) {
    // Never log or do other work while holding the global frame allocator lock.
    let _ = with_frame_allocator(|frames| address_space.destroy(frames));
}

/// Starts `actor` and returns its address.
///
/// A free slot is claimed, the image is built for it, the inbox is initialised, and the actor
/// is published and handed to the scheduler. The actor is visible to the scheduler only after
/// everything it needs is in place. On any failure everything taken is given back.
///
/// # Errors
///
/// Returns [`SpawnError`] when the budget exceeds the ceiling, the image is invalid, memory runs
/// out, no slot is free, or the scheduler rejects the actor.
pub fn enqueue(actor: Actor<'_>) -> Result<LocalAddress, SpawnError> {
    check_budget(actor.configuration())?;

    let image = *actor.image();
    let (name, configuration, parent_address) = actor.into_parts();

    for slot_index in 0..actor_table_length() {
        let Some(slot) = actor_slot(slot_index) else {
            break;
        };
        let mut control_block = slot.lock();
        if control_block.state != State::Free {
            continue;
        }

        let (Ok(slot_number), Ok(address_space_identifier)) =
            (u32::try_from(slot_index), u16::try_from(slot_index + 1))
        else {
            drop(control_block);
            error!(
                EarlyConsole,
                "Actor creation failed: slot {slot_index} exceeds the address range"
            );
            return Err(SpawnError::TableFull);
        };

        assert!(control_block.inbox.is_none() && control_block.address_space.is_none());
        assert!(!control_block.queued && control_block.pending_message_releases == 0);

        let prepared = match build_image(&image, slot_index as u64 * PRIVATE_VALUE_STRIDE) {
            Ok(prepared) => prepared,
            Err(spawn_error) => {
                drop(control_block);
                error!(
                    EarlyConsole,
                    "Actor creation failed in slot {slot_index}: {spawn_error:?}"
                );
                return Err(spawn_error);
            }
        };

        // From here on every path either returns or consumes `prepared.address_space`.
        let frame_address = match prepared.user_stack_top {
            Some(stack_top) => {
                prepare_first_user_frame(slot_index, prepared.entry_point, stack_top)
            }
            None => prepare_first_frame(slot_index, prepared.entry_point),
        };
        let Some(frame_address) = frame_address else {
            drop(control_block);
            discard(prepared.address_space);
            error!(
                EarlyConsole,
                "Actor creation failed in slot {slot_index}: initial frame unavailable"
            );
            return Err(SpawnError::StackUnavailable);
        };

        // Identifies this exact incarnation of the slot.
        let address = LocalAddress::new(slot_number, control_block.generation_counter);

        invalidate_address_space_identifier(address_space_identifier);

        control_block.inbox = Some(Inbox::new(address, inbox_byte_budget(&configuration)));
        control_block.exit_cause = None;
        control_block.parent_address = parent_address;
        control_block.name = name;
        control_block.configuration = Some(configuration);
        control_block.saved_frame_address = frame_address;
        control_block.address_space = Some(prepared.address_space);
        control_block.address_space_identifier = address_space_identifier;

        // Publish only after everything above is in place.
        control_block.transition_to(State::Runnable);

        // Release the actor lock before interacting with the scheduler.
        drop(control_block);

        if enqueue_runnable(slot_index) {
            return Ok(address);
        }

        // Roll back, so the actor does not sit `Runnable` in no queue forever.
        let mut control_block = slot.lock();
        control_block.inbox = None;
        control_block.configuration = None;
        control_block.parent_address = None;
        control_block.name = Name::UNNAMED;
        control_block.saved_frame_address = 0;
        control_block.address_space_identifier = 0;
        if let Some(address_space) = control_block.address_space.take() {
            drop(control_block);
            discard(address_space);
            control_block = slot.lock();
        }
        // The address was never handed out, but bumping keeps "a reused slot is a new actor" true.
        control_block.generation_counter = control_block.generation_counter.wrapping_add(1);
        control_block.transition_to(State::Free);
        return Err(SpawnError::SchedulerRejected);
    }

    error!(EarlyConsole, "Actor creation failed: actor table is full");
    Err(SpawnError::TableFull)
}

/// Runs `operation` on the control block at `slot_index`, holding its lock.
pub fn with_control_block<Output>(
    slot_index: usize,
    operation: impl FnOnce(&mut ControlBlock) -> Output,
) -> Option<Output> {
    let slot = actor_slot(slot_index)?;
    let mut control_block = slot.lock();
    Some(operation(&mut control_block))
}
