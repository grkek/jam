//! GICv2 interrupt controller using the selected platform's register addresses.

use crate::memory::address_translation::physical_to_virtual;
use core::ptr::{read_volatile, write_volatile};

/// Kernel virtual base address of the GIC distributor.
const DISTRIBUTOR_BASE_ADDRESS: usize =
    physical_to_virtual(crate::platform::current::GIC_DISTRIBUTOR_PHYSICAL_BASE_ADDRESS);
/// Kernel virtual base address of the GIC CPU interface.
const CPU_INTERFACE_BASE_ADDRESS: usize =
    physical_to_virtual(crate::platform::current::GIC_CPU_INTERFACE_PHYSICAL_BASE_ADDRESS);

/// Distributor control register offset.
const DISTRIBUTOR_CONTROL_OFFSET: usize = 0x000;
/// Set-enable register bank offset.
const DISTRIBUTOR_SET_ENABLE_OFFSET: usize = 0x100;
/// Priority register bank offset.
const DISTRIBUTOR_PRIORITY_OFFSET: usize = 0x400;
/// CPU interface control register offset.
const CPU_INTERFACE_CONTROL_OFFSET: usize = 0x000;
/// CPU interface priority mask offset.
const CPU_INTERFACE_PRIORITY_MASK_OFFSET: usize = 0x004;
/// Interrupt acknowledge register offset.
const CPU_INTERFACE_ACKNOWLEDGE_OFFSET: usize = 0x00c;
/// End-of-interrupt register offset.
const CPU_INTERFACE_END_OF_INTERRUPT_OFFSET: usize = 0x010;

/// Value that enables the controller.
const ENABLE: u32 = 1;
/// Priority mask allowing all implemented priorities.
const ACCEPT_ALL_PRIORITIES: u32 = 0xff;
/// Priority assigned to the scheduler timer.
const TIMER_PRIORITY: u8 = 0x80;
/// Mask used to extract an interrupt identifier.
const INTERRUPT_IDENTIFIER_MASK: u32 = 0x3ff;
/// Identifiers from here up mean "nothing pending".
const FIRST_SPURIOUS_IDENTIFIER: u32 = 1020;

/// The EL1 physical timer's private peripheral interrupt.
pub const TIMER_INTERRUPT_IDENTIFIER: u32 = 30;

/// Writes an aligned 32-bit GIC register.
fn write_register(address: usize, value: u32) {
    // SAFETY: The GIC registers are memory-mapped at fixed addresses on QEMU `virt`, mapped as
    // device memory, 32 bits wide and aligned. Volatile access is how devices are written.
    unsafe { write_volatile(address as *mut u32, value) };
}

/// Reads an aligned 32-bit GIC register.
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
