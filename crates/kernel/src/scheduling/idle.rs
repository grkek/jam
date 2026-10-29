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

/// Byte pattern used to detect interrupt-stack consumption.
const STACK_PATTERN: u8 = 0xA5;

unsafe extern "C" {
    /// First address of the interrupt-stack region, defined by the linker script.
    static __interrupt_stacks_bottom: u8;

    /// First address beyond the interrupt-stack region, defined by the linker script.
    static __interrupt_stacks_top: u8;
}

/// Fills every interrupt stack with a recognizable pattern for usage tracking.
///
/// Must be called once by the boot core before secondary cores start taking
/// interrupts. Each core subsequently overwrites the pattern as its stack grows.
pub fn initialise_interrupt_stack_tracking() {
    let bottom = &raw const __interrupt_stacks_bottom as usize;
    let top = &raw const __interrupt_stacks_top as usize;

    let stride = (top - bottom) / CORE_MAXIMUM;
    let stack_size = stride - 4096;

    for core_index in 0..CORE_MAXIMUM {
        let stack_bottom = bottom + core_index * stride + 4096;

        unsafe {
            core::ptr::write_bytes(stack_bottom as *mut u8, STACK_PATTERN, stack_size);
        }
    }
}

/// Returns the estimated maximum interrupt-stack usage and capacity for one core.
///
/// Scans upward from the bottom of the stack until the first byte that no
/// longer matches the initialization pattern. Since stacks grow downward,
/// this estimates the deepest stack usage observed since initialization.
///
/// The measurement is approximate because written bytes may coincidentally
/// match the pattern. It must not be used as a memory-safety guarantee.
pub fn interrupt_stack_high_water_bytes(core_index: usize) -> (usize, usize) {
    assert!(core_index < CORE_MAXIMUM);

    let bottom = &raw const __interrupt_stacks_bottom as usize;
    let top = &raw const __interrupt_stacks_top as usize;

    let stride = (top - bottom) / CORE_MAXIMUM;

    let stack_size = stride - 4096;
    let stack_bottom = bottom + core_index * stride + 4096;
    let stack_top = stack_bottom + stack_size;

    let mut cursor = stack_bottom;

    // Read the pattern without allowing the compiler to eliminate or
    // combine accesses to the stack memory being inspected.
    while cursor < stack_top {
        // SAFETY: The cursor remains inside the linker-reserved
        // interrupt-stack region for the specified core.
        let byte = unsafe { core::ptr::read_volatile(cursor as *const u8) };

        if byte != STACK_PATTERN {
            break;
        }

        cursor += 1;
    }

    (stack_top - cursor, stack_size)
}

/// Storage for all per-core idle stacks, aligned to 16 bytes.
#[repr(C, align(16))]
struct IdleStackArea(UnsafeCell<[u8; CORE_MAXIMUM * IDLE_STACK_BYTE_COUNT]>);

// SAFETY: Each core uses only its own idle stack, and only after `install_idle_frames`.
unsafe impl Sync for IdleStackArea {}

/// Dedicated idle-stack storage shared across cores without overlapping slices.
static IDLE_STACKS: IdleStackArea =
    IdleStackArea(UnsafeCell::new([0; CORE_MAXIMUM * IDLE_STACK_BYTE_COUNT]));

/// Parks the current core until an event arrives.
///
/// This function never returns. The scheduler may interrupt the idle context
/// and resume another actor through the exception-return mechanism.
extern "C" fn idle_loop() -> ! {
    loop {
        processor::wait_for_event();
    }
}

/// Returns the exclusive upper boundary of the specified core's idle stack.
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
