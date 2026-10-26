//! The generic interrupt controller (`GICv2`), as `QEMU` `virt` provides it.

use crate::memory::address_translation::KERNEL_VIRTUAL_OFFSET;
use core::ptr::{read_volatile, write_volatile};

const DISTRIBUTOR_BASE_ADDRESS: usize = KERNEL_VIRTUAL_OFFSET + 0x0800_0000;
const CPU_INTERFACE_BASE_ADDRESS: usize = KERNEL_VIRTUAL_OFFSET + 0x0801_0000;

const DISTRIBUTOR_CONTROL_OFFSET: usize = 0x000;
const DISTRIBUTOR_SET_ENABLE_OFFSET: usize = 0x100;
const DISTRIBUTOR_PRIORITY_OFFSET: usize = 0x400;
const CPU_INTERFACE_CONTROL_OFFSET: usize = 0x000;
const CPU_INTERFACE_PRIORITY_MASK_OFFSET: usize = 0x004;
const CPU_INTERFACE_ACKNOWLEDGE_OFFSET: usize = 0x00c;
const CPU_INTERFACE_END_OF_INTERRUPT_OFFSET: usize = 0x010;

const ENABLE: u32 = 1;
const ACCEPT_ALL_PRIORITIES: u32 = 0xff;
const TIMER_PRIORITY: u8 = 0x80;
const INTERRUPT_IDENTIFIER_MASK: u32 = 0x3ff;
/// Identifiers from here up mean "nothing pending".
const FIRST_SPURIOUS_IDENTIFIER: u32 = 1020;

/// The EL1 physical timer's private peripheral interrupt.
pub const TIMER_INTERRUPT_IDENTIFIER: u32 = 30;

fn write_register(address: usize, value: u32) {
    // SAFETY: The GIC registers are memory-mapped at fixed addresses on QEMU `virt`, mapped as
    // device memory, 32 bits wide and aligned. Volatile access is how devices are written.
    unsafe { write_volatile(address as *mut u32, value) };
}

fn read_register(address: usize) -> u32 {
    // SAFETY: As in `write_register`.
    unsafe { read_volatile(address as *const u32) }
}

/// Turns the shared distributor on. Done once, by the boot core.
pub fn initialise_distributor() {
    write_register(
        DISTRIBUTOR_BASE_ADDRESS + DISTRIBUTOR_CONTROL_OFFSET,
        ENABLE,
    );
}

/// Prepares the calling core to receive the timer interrupt. Every core does this for itself.
pub fn initialise_core_interface() {
    // SAFETY: A byte write to the priority register of a private interrupt, which is banked per
    // core, same mapping and volatility rules as the word registers.
    unsafe {
        write_volatile(
            (DISTRIBUTOR_BASE_ADDRESS
                + DISTRIBUTOR_PRIORITY_OFFSET
                + TIMER_INTERRUPT_IDENTIFIER as usize) as *mut u8,
            TIMER_PRIORITY,
        );
    }
    write_register(
        DISTRIBUTOR_BASE_ADDRESS + DISTRIBUTOR_SET_ENABLE_OFFSET,
        1 << TIMER_INTERRUPT_IDENTIFIER,
    );
    write_register(
        CPU_INTERFACE_BASE_ADDRESS + CPU_INTERFACE_PRIORITY_MASK_OFFSET,
        ACCEPT_ALL_PRIORITIES,
    );
    write_register(
        CPU_INTERFACE_BASE_ADDRESS + CPU_INTERFACE_CONTROL_OFFSET,
        ENABLE,
    );
}

/// Takes the highest-priority pending interrupt, or `None` when it was spurious.
#[must_use]
pub fn acknowledge_interrupt() -> Option<u32> {
    let identifier = read_register(CPU_INTERFACE_BASE_ADDRESS + CPU_INTERFACE_ACKNOWLEDGE_OFFSET)
        & INTERRUPT_IDENTIFIER_MASK;
    (identifier < FIRST_SPURIOUS_IDENTIFIER).then_some(identifier)
}

/// Tells the controller the interrupt has been handled.
pub fn end_of_interrupt(identifier: u32) {
    write_register(
        CPU_INTERFACE_BASE_ADDRESS + CPU_INTERFACE_END_OF_INTERRUPT_OFFSET,
        identifier,
    );
}
