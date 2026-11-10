//! The table of every actor on this node.
//!
//! The table is built at boot from physical frames, so its size follows the machine's memory.
//! Each chunk is one frame of control blocks; a paged directory points at the chunks.

use core::sync::atomic::{AtomicUsize, Ordering};

use foundation::{address::LocalAddress, configuration::Configuration};

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
        user_memory::{TEST_CODE_ADDRESS, UserMapError},
        user_program::{
            build_elf_user_address_space, build_user_address_space, elf_user_stack_top,
            user_stack_top,
        },
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
}

/// What a user actor needs to start: its address space, its entry point, and its stack top.
type UserImage = (AddressSpace, usize, usize);

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

/// Starts an actor at `entry_point` in kernel mode, in its own address space.
///
/// initialises the actor's address space, execution context, and inbox
/// before publishing it to the scheduler.
///
/// # Errors
///
/// Returns [`SpawnError`] when the budget exceeds the system ceiling, or when no
/// actor-table slot, initial kernel stack, or address-space memory is available.
pub fn spawn_kernel_actor(
    entry_point: extern "C" fn() -> !,
    parent_address: Option<LocalAddress>,
    configuration: Configuration,
) -> Result<LocalAddress, SpawnError> {
    check_budget(&configuration)?;

    for slot_index in 0..actor_table_length() {
        let Some(slot) = actor_slot(slot_index) else {
            break;
        };
        let mut control_block = slot.lock();

        if control_block.state != State::Free {
            continue;
        }

        assert!(control_block.inbox.is_none() && control_block.address_space.is_none());
        assert!(!control_block.queued && control_block.pending_message_releases == 0);

        let slot_number = u32::try_from(slot_index).map_err(|_| SpawnError::TableFull)?;

        let address_space_identifier =
            u16::try_from(slot_index + 1).map_err(|_| SpawnError::TableFull)?;

        let address = LocalAddress::new(slot_number, control_block.generation_counter);

        let frame_address = prepare_first_frame(slot_index, entry_point as usize)
            .ok_or(SpawnError::StackUnavailable)?;

        let address_space = with_frame_allocator(|frame_allocator| {
            build_address_space_with_private_page(
                frame_allocator,
                slot_index as u64 * PRIVATE_VALUE_STRIDE,
            )
        })
        .flatten()
        .ok_or(SpawnError::OutOfMemory)?;

        invalidate_address_space_identifier(address_space_identifier);

        // initialise the actor's resources before making it runnable.
        control_block.inbox = Some(Inbox::new(address, inbox_byte_budget(&configuration)));

        control_block.exit_cause = None;
        control_block.parent_address = parent_address;
        control_block.configuration = Some(configuration);
        control_block.saved_frame_address = frame_address;
        control_block.address_space = Some(address_space);
        control_block.address_space_identifier = address_space_identifier;

        // Publish the fully initialised actor.
        control_block.transition_to(State::Runnable);

        // Release the control-block lock before scheduling.
        drop(control_block);

        assert!(enqueue_runnable(slot_index), "Queue rejected new actor");

        return Ok(address);
    }

    Err(SpawnError::TableFull)
}

/// Finds a free actor-table slot, constructs its user address space,
/// initialises its inbox, and makes the actor runnable.
///
/// The actor is published to the scheduler only after all required
/// resources have been successfully initialised.
///
/// # Errors
///
/// Returns [`SpawnError`] when the budget exceeds the system ceiling, when no actor
/// slot, kernel stack, or physical memory is available, or when the user image
/// cannot be constructed.
fn spawn_user(
    parent_address: Option<LocalAddress>,
    configuration: Configuration,
    initial_argument: u64,
    build_image: impl FnOnce(&mut FrameAllocator, u64) -> Result<UserImage, UserMapError>,
) -> Result<LocalAddress, SpawnError> {
    if let Err(spawn_error) = check_budget(&configuration) {
        error!(
            EarlyConsole,
            "Actor creation failed: requested budget exceeds the system ceiling"
        );
        return Err(spawn_error);
    }

    for slot_index in 0..actor_table_length() {
        let Some(slot) = actor_slot(slot_index) else {
            break;
        };
        let mut control_block = slot.lock();

        if control_block.state != State::Free {
            continue;
        }

        let slot_number = match u32::try_from(slot_index) {
            Ok(slot_number) => slot_number,
            Err(_) => {
                drop(control_block);
                error!(
                    EarlyConsole,
                    "Actor creation failed: slot {slot_index} exceeds the address range"
                );
                return Err(SpawnError::TableFull);
            }
        };

        let address_space_identifier = match u16::try_from(slot_index + 1) {
            Ok(address_space_identifier) => address_space_identifier,
            Err(_) => {
                drop(control_block);
                error!(
                    EarlyConsole,
                    "Actor creation failed: no address-space identifier for slot {slot_index}"
                );
                return Err(SpawnError::TableFull);
            }
        };

        // Identify this exact incarnation of the actor-table slot.
        let address = LocalAddress::new(slot_number, control_block.generation_counter);

        // Never log success while holding the global frame allocator lock.
        // Capture the result first so error reporting cannot retain the allocator guard.
        let image_result = with_frame_allocator(|frame_allocator| {
            build_image(frame_allocator, slot_index as u64 * PRIVATE_VALUE_STRIDE)
        });

        let (address_space, entry_point, stack_top) = match image_result {
            Some(Ok(user_image)) => user_image,
            Some(Err(user_map_error)) => {
                let spawn_error = match user_map_error {
                    UserMapError::OutOfFrames => SpawnError::OutOfMemory,
                    _ => SpawnError::InvalidExecutable,
                };
                drop(control_block);
                error!(
                    EarlyConsole,
                    "Actor creation failed in slot {slot_index}: user image error {user_map_error:?}"
                );
                return Err(spawn_error);
            }
            None => {
                drop(control_block);
                error!(
                    EarlyConsole,
                    "Actor creation failed in slot {slot_index}: frame allocator is unavailable"
                );
                return Err(SpawnError::OutOfMemory);
            }
        };

        // Prepare the initial EL0 execution context.
        let Some(frame_address) =
            prepare_first_user_frame(slot_index, entry_point, stack_top, initial_argument)
        else {
            let _ = with_frame_allocator(|frame_allocator| address_space.destroy(frame_allocator));
            drop(control_block);
            error!(
                EarlyConsole,
                "Actor creation failed in slot {slot_index}: initial EL0 frame unavailable"
            );
            return Err(SpawnError::StackUnavailable);
        };

        invalidate_address_space_identifier(address_space_identifier);

        // Initialise the inbox before publishing the actor.
        control_block.inbox = Some(Inbox::new(address, inbox_byte_budget(&configuration)));

        // Initialise actor lifecycle and execution resources.
        control_block.exit_cause = None;
        control_block.parent_address = parent_address;
        control_block.configuration = Some(configuration);
        control_block.saved_frame_address = frame_address;
        control_block.address_space = Some(address_space);
        control_block.address_space_identifier = address_space_identifier;

        assert_eq!(
            unsafe { core::ptr::read_volatile((frame_address + 248) as *const u64) },
            entry_point as u64,
            "first frame wrong at spawn"
        );

        // Publish only after all initialization has completed.
        control_block.transition_to(State::Runnable);

        // Release the actor lock before interacting with the scheduler.
        drop(control_block);

        if !enqueue_runnable(slot_index) {
            error!(
                EarlyConsole,
                "Actor creation failed to enqueue runnable actor in slot {slot_index}"
            );
            // Preserve the existing return contract. A separate scheduler change is required
            // to roll back a fully published actor after enqueue failure.
        }

        return Ok(address);
    }

    error!(EarlyConsole, "Actor creation failed: actor table is full");
    Err(SpawnError::TableFull)
}

/// Starts an EL0 actor from a statically linked AArch64 ELF64 executable.
///
/// Maps the executable's loadable segments and user stack, then prepares the first user frame
/// with the ELF entry point and the ELF stack top. `program` must be the complete ELF image.
///
/// # Errors
///
/// Returns [`SpawnError::InvalidExecutable`] when the ELF is malformed or has an unsupported
/// mapping, [`SpawnError::OutOfMemory`] when frame allocation fails,
/// [`SpawnError::BudgetExceedsCeiling`] when the budget is too large, or the corresponding
/// error when the table or stack is unavailable.
pub fn spawn_user_actor(
    program: &'static [u8],
    parent_address: Option<LocalAddress>,
    configuration: Configuration,
) -> Result<LocalAddress, SpawnError> {
    spawn_user_actor_with_argument(program, parent_address, configuration, 0)
}

/// Starts a known ELF with a kernel-authenticated scalar in its initial x0.
/// The value is installed before the actor becomes runnable; it is not obtained
/// from an unauthenticated first inbox message.
///
/// # Errors
/// Returns the same image, stack, budget, and allocation errors as `spawn_user_actor`.
pub fn spawn_user_actor_with_argument(
    program: &'static [u8],
    parent_address: Option<LocalAddress>,
    configuration: Configuration,
    initial_argument: u64,
) -> Result<LocalAddress, SpawnError> {
    spawn_user(
        parent_address,
        configuration,
        initial_argument,
        |frame_allocator, private_value| {
            let (address_space, entry_point) =
                build_elf_user_address_space(frame_allocator, private_value, program)?;
            let entry_point =
                usize::try_from(entry_point).map_err(|_| UserMapError::InvalidRegion)?;
            Ok((address_space, entry_point, elf_user_stack_top()))
        },
    )
}

/// Starts an EL0 actor from raw machine code. For self-tests, not an ELF loader.
///
/// # Errors
///
/// Returns [`SpawnError`] when no slot or memory is available, or the budget is too large.
pub fn spawn_raw_user_actor(
    program: &'static [u8],
    parent_address: Option<LocalAddress>,
    configuration: Configuration,
) -> Result<LocalAddress, SpawnError> {
    spawn_user(
        parent_address,
        configuration,
        0,
        |frame_allocator, private_value| {
            let address_space = build_user_address_space(frame_allocator, private_value, program)?;
            Ok((address_space, TEST_CODE_ADDRESS, user_stack_top()))
        },
    )
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
