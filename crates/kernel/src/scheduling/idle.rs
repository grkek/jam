//! Per-core idle contexts and interrupt-stack usage tracking.

use foundation::page_table_entry::PAGE_BYTE_COUNT;

use crate::{
    architecture::aarch64::{exceptions::TrapFrame, processor},
    scheduling::core_local::{CORE_MAXIMUM, core_locals},
};

/// Size of each core's idle stack.
const IDLE_STACK_BYTE_COUNT: usize = 8 * 1024;
const STACK_GUARD_BYTE_COUNT: usize = PAGE_BYTE_COUNT;
const IDLE_STACK_STRIDE_BYTE_COUNT: usize = IDLE_STACK_BYTE_COUNT + STACK_GUARD_BYTE_COUNT;
const SAVED_STATE_EL1_WITH_INTERRUPTS: u64 = 0b0101;
const INTERRUPT_STACK_PATTERN: u8 = 0xA5;

unsafe extern "C" {
    /*
        Pointers to the top and bottom of idle stacks.
    */
    static __idle_stacks_bottom: u8;
    static __idle_stacks_top: u8;

    /*
        Pointers to the top and bottom of interrupt stacks.
    */
    static __interrupt_stacks_bottom: u8;
    static __interrupt_stacks_top: u8;
}

// Returns the mapped boundaries of a core's interrupt stack, excluding its guard page.
fn interrupt_stack_bounds(core_index: usize) -> (usize, usize) {
    assert!(core_index < CORE_MAXIMUM);

    let region_bottom = &raw const __interrupt_stacks_bottom as usize;
    let region_top = &raw const __interrupt_stacks_top as usize;
    let region_byte_count = region_top
        .checked_sub(region_bottom)
        .expect("Invalid interrupt-stack region");

    assert_eq!(region_bottom % PAGE_BYTE_COUNT, 0);
    assert_eq!(region_byte_count % CORE_MAXIMUM, 0);

    let stride = region_byte_count / CORE_MAXIMUM;
    assert!(stride > STACK_GUARD_BYTE_COUNT);
    assert_eq!(stride % PAGE_BYTE_COUNT, 0);

    let stack_bottom = region_bottom + core_index * stride + STACK_GUARD_BYTE_COUNT;
    let stack_top = region_bottom + (core_index + 1) * stride;
    (stack_bottom, stack_top)
}

/// Initializes interrupt-stack watermarks before other cores enable interrupts.
pub fn initialise_interrupt_stack_tracking() {
    for core_index in 0..CORE_MAXIMUM {
        let (stack_bottom, stack_top) = interrupt_stack_bounds(core_index);

        // SAFETY: Only mapped, inactive interrupt-stack memory is initialized.
        unsafe {
            core::ptr::write_bytes(
                stack_bottom as *mut u8,
                INTERRUPT_STACK_PATTERN,
                stack_top - stack_bottom,
            );
        }
    }
}

/// Estimates the maximum interrupt-stack consumption since initialization.
#[must_use]
pub fn interrupt_stack_high_water_bytes(core_index: usize) -> (usize, usize) {
    let (stack_bottom, stack_top) = interrupt_stack_bounds(core_index);
    let mut cursor = stack_bottom;

    while cursor < stack_top {
        // SAFETY: The cursor stays inside the mapped interrupt-stack region.
        let value = unsafe { core::ptr::read_volatile(cursor as *const u8) };
        if value != INTERRUPT_STACK_PATTERN {
            break;
        }
        cursor += 1;
    }

    (stack_top - cursor, stack_top - stack_bottom)
}

// Returns the exclusive upper boundary of the linker-reserved idle stack.
fn idle_stack_top(core_index: usize) -> usize {
    assert!(core_index < CORE_MAXIMUM);

    let region_bottom = &raw const __idle_stacks_bottom as usize;
    let region_top = &raw const __idle_stacks_top as usize;

    assert_eq!(region_bottom % PAGE_BYTE_COUNT, 0);
    assert_eq!(
        region_top.checked_sub(region_bottom),
        Some(CORE_MAXIMUM * IDLE_STACK_STRIDE_BYTE_COUNT),
        "Invalid idle-stack region"
    );

    region_bottom + (core_index + 1) * IDLE_STACK_STRIDE_BYTE_COUNT
}

// Waits for an event without consuming CPU time in a busy loop.
extern "C" fn idle_loop() -> ! {
    loop {
        processor::wait_for_event();
    }
}

/// Prepares one independently guarded idle trap frame for each execution core.
pub fn install_idle_frames() {
    assert!(size_of::<TrapFrame>() <= IDLE_STACK_BYTE_COUNT);

    for core_local in core_locals() {
        let core_index = core_local.core_index();
        let frame_address = idle_stack_top(core_index) - size_of::<TrapFrame>();
        assert_eq!(frame_address % align_of::<TrapFrame>(), 0);

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

        // SAFETY: Boot installs these frames before idle contexts can run.
        unsafe {
            core::ptr::write(frame_address as *mut TrapFrame, frame);
        }

        core_local.set_idle_frame_address(frame_address);
    }
}
