//! Per-core idle context: what a core resumes when no actor is runnable.

use core::cell::UnsafeCell;

use crate::{
    architecture::aarch64::{exceptions::TrapFrame, processor},
    scheduling::core_local::{CORE_MAXIMUM, core_locals},
};

/// Size of each core's idle stack.
const IDLE_STACK_BYTE_COUNT: usize = 8 * 1024;
/// Saved state for EL1 using its own stack pointer, with interrupts enabled.
const SAVED_STATE_EL1_WITH_INTERRUPTS: u64 = 0b0101;

#[repr(C, align(16))]
struct IdleStackArea(UnsafeCell<[u8; CORE_MAXIMUM * IDLE_STACK_BYTE_COUNT]>);

// SAFETY: Each core uses only its own idle stack, and only after `install_idle_frames`.
unsafe impl Sync for IdleStackArea {}

static IDLE_STACKS: IdleStackArea =
    IdleStackArea(UnsafeCell::new([0; CORE_MAXIMUM * IDLE_STACK_BYTE_COUNT]));

/// Parks the core until an interrupt arrives. Resumed through the idle trap frame.
extern "C" fn idle_loop() -> ! {
    loop {
        processor::wait_for_event();
    }
}

fn idle_stack_top(core_index: usize) -> usize {
    let base = IDLE_STACKS.0.get() as usize;
    base + (core_index + 1) * IDLE_STACK_BYTE_COUNT
}

/// Writes an idle trap frame for every core and records it on that core's local data.
///
/// Call once on the boot core before secondary cores start taking ticks. Each secondary
/// core already has its `CoreLocal`; only the frame address is stored.
pub fn install_idle_frames() {
    for core_local in core_locals() {
        let core_index = core_local.core_index();
        let frame_address = idle_stack_top(core_index) - size_of::<TrapFrame>();
        let frame = TrapFrame {
            general_registers: [0; 31],
            exception_link_register: idle_loop as usize as u64,
            saved_program_status_register: SAVED_STATE_EL1_WITH_INTERRUPTS,
            exception_syndrome_register: 0,
            fault_address_register: 0,
            vector_index: 0,
            stack_pointer_el0: 0,
            _padding: 0,
        };
        // SAFETY: This core's idle stack is unused until the first schedule into idle.
        // The top is 16-byte aligned and the frame size is a multiple of 16.
        unsafe {
            core::ptr::write(frame_address as *mut TrapFrame, frame);
        }
        core_local.set_idle_frame_address(frame_address);
    }
}
