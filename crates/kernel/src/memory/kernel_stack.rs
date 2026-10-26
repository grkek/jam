//! Kernel stacks for actors that run in kernel mode, one per actor slot.

use core::cell::UnsafeCell;

use crate::{actors::table::ACTOR_MAXIMUM, architecture::aarch64::exceptions::TrapFrame};

/// Size of each actor's kernel stack.
pub const KERNEL_STACK_BYTE_COUNT: usize = 16 * 1024;
/// Saved state for EL1 using its own stack pointer, with interrupts enabled.
const SAVED_STATE_EL1_WITH_INTERRUPTS: u64 = 0b0101;

#[repr(C, align(16))]
struct StackArea(UnsafeCell<[u8; ACTOR_MAXIMUM * KERNEL_STACK_BYTE_COUNT]>);

// SAFETY: Each slot's stack is used only by the actor in that slot, and by the code that
// prepares it while the slot is being spawned and nothing runs on it yet.
unsafe impl Sync for StackArea {}

static KERNEL_STACKS: StackArea = StackArea(UnsafeCell::new(
    [0; ACTOR_MAXIMUM * KERNEL_STACK_BYTE_COUNT],
));

/// The highest address of the stack belonging to `slot_index`.
#[must_use]
pub fn stack_top_address(slot_index: usize) -> Option<usize> {
    if slot_index >= ACTOR_MAXIMUM {
        return None;
    }
    let base_address = KERNEL_STACKS.0.get() as usize;
    Some(base_address + (slot_index + 1) * KERNEL_STACK_BYTE_COUNT)
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
    };
    // SAFETY: The address lies inside this slot's stack, which nothing uses yet. The stack top is
    // 16-byte aligned and the frame size is a multiple of 16, so the frame is aligned for writing.
    unsafe {
        core::ptr::write(frame_address as *mut TrapFrame, frame);
    }
    Some(frame_address)
}
