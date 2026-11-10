//! Kernel stacks for actors that run in kernel mode, one per actor slot.
//!
//! The stacks are built at boot from physical frames, so their number follows the actor table.

use crate::{
    architecture::aarch64::exceptions::TrapFrame,
    memory::{
        address_translation::physical_to_virtual,
        frame_allocator::FrameAllocator,
        paged_directory::{DirectoryError, PagedDirectory},
    },
};

const FRAME_BYTE_COUNT: usize = 4096;

/// Size of each actor's kernel stack.
pub const KERNEL_STACK_BYTE_COUNT: usize = 16 * 1024;
/// Physically adjacent frames behind one stack.
const FRAMES_PER_STACK: usize = KERNEL_STACK_BYTE_COUNT / FRAME_BYTE_COUNT;
/// Saved state for EL1 using its own stack pointer, with interrupts enabled.
const SAVED_STATE_EL1_WITH_INTERRUPTS: u64 = 0b0101;
/// Saved state for EL0 using SP_EL0, with interrupts enabled.
const SAVED_STATE_EL0_WITH_INTERRUPTS: u64 = 0b0000;

const _: () = assert!(
    KERNEL_STACK_BYTE_COUNT % FRAME_BYTE_COUNT == 0 && KERNEL_STACK_BYTE_COUNT % 16 == 0,
    "a kernel stack must be a whole number of frames and 16-byte aligned"
);

/// Base address of every stack. The stacks themselves come from the frame allocator.
static STACK_DIRECTORY: PagedDirectory = PagedDirectory::new();

/// Why the kernel stacks could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackError {
    /// The stack directory could not be built.
    Directory(DirectoryError),
    /// The physical frame allocator ran out of frames.
    OutOfFrames,
    /// The frame allocator did not hand out adjacent frames for a stack.
    FramesNotContiguous,
}

/// Takes the frames of one stack and returns the address of the first.
///
/// Boot runs before the allocator is fragmented, so consecutive allocations are expected to
/// be adjacent. The check turns a violated assumption into a boot error instead of corruption.
fn allocate_stack_frames(frame_allocator: &mut FrameAllocator) -> Result<usize, StackError> {
    let first_frame = frame_allocator
        .allocate_frame()
        .ok_or(StackError::OutOfFrames)?;

    for frame_offset in 1..FRAMES_PER_STACK {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(StackError::OutOfFrames)?;
        if frame != first_frame + frame_offset * FRAME_BYTE_COUNT {
            return Err(StackError::FramesNotContiguous);
        }
    }

    Ok(first_frame)
}

/// Builds `stack_count` kernel stacks and returns how many frames they took.
///
/// Call once at boot, while the caller still owns the frame allocator and before the first spawn.
/// Frames already taken are not returned on failure, because boot treats that as fatal.
///
/// # Errors
///
/// Returns [`StackError`] when the directory cannot be built or the frame allocator
/// cannot supply adjacent frames.
pub fn initialise_kernel_stacks(
    frame_allocator: &mut FrameAllocator,
    stack_count: usize,
) -> Result<usize, StackError> {
    let directory_frame_count = STACK_DIRECTORY
        .initialise(frame_allocator, stack_count)
        .map_err(StackError::Directory)?;

    for stack_index in 0..stack_count {
        let frame_address = allocate_stack_frames(frame_allocator)?;
        let base_address = physical_to_virtual(frame_address) as usize;
        STACK_DIRECTORY.set(stack_index, base_address);
    }

    Ok(directory_frame_count + stack_count * FRAMES_PER_STACK)
}

/// The highest address of the stack belonging to `slot_index`.
#[must_use]
pub fn stack_top_address(slot_index: usize) -> Option<usize> {
    Some(STACK_DIRECTORY.get(slot_index)? + KERNEL_STACK_BYTE_COUNT)
}

/// Writes an EL0 frame with the user PC, stack, and boot-authenticated x0 argument.
#[must_use]
pub fn prepare_first_user_frame(
    slot_index: usize,
    entry_point_address: usize,
    user_stack_top: usize,
    initial_argument: u64,
) -> Option<usize> {
    let frame_address = stack_top_address(slot_index)? - size_of::<TrapFrame>();
    let mut general_registers = [0; 31];
    if let Some(register) = general_registers.first_mut() {
        *register = initial_argument;
    }
    let frame = TrapFrame {
        general_registers,
        exception_link_register: entry_point_address as u64,
        saved_program_status_register: SAVED_STATE_EL0_WITH_INTERRUPTS,
        exception_syndrome_register: 0,
        fault_address_register: 0,
        vector_index: 0,
        stack_pointer_el0: user_stack_top as u64,
        _padding: 0,
    };
    // SAFETY: same as prepare_first_frame — slot stack is unused and 16-byte aligned.
    unsafe {
        core::ptr::write(frame_address as *mut TrapFrame, frame);
    }
    Some(frame_address)
}

/// Writes the frame a new actor resumes from at the top of its stack and returns its address.
///
/// Resuming that frame starts execution at `entry_point_address` on this stack.
#[must_use]
pub fn prepare_first_frame(slot_index: usize, entry_point_address: usize) -> Option<usize> {
    let frame_address = stack_top_address(slot_index)? - size_of::<TrapFrame>();
    let frame = TrapFrame {
        general_registers: [0; 31],
        exception_link_register: entry_point_address as u64,
        saved_program_status_register: SAVED_STATE_EL1_WITH_INTERRUPTS,
        exception_syndrome_register: 0,
        fault_address_register: 0,
        vector_index: 0,
        stack_pointer_el0: 0,
        _padding: 0,
    };
    // SAFETY: The address lies inside this slot's stack, which nothing uses yet. The stack top is
    // 16-byte aligned and the frame size is a multiple of 16, so the frame is aligned for writing.
    unsafe {
        core::ptr::write(frame_address as *mut TrapFrame, frame);
    }
    Some(frame_address)
}
