//! GICv2 interrupt controller using the selected platform's register addresses.

use crate::memory::address_translation::physical_to_virtual;
use crate::scheduling::core_local::{CORE_COUNT, current_core_local};
use core::{
    ptr::{read_volatile, write_volatile},
    sync::atomic::{AtomicU32, Ordering},
};

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
/// Banked interrupt group register for private interrupts.
const DISTRIBUTOR_GROUP_OFFSET: usize = 0x080;
/// Banked target bytes identify the calling GICv2 CPU interface.
const DISTRIBUTOR_TARGET_OFFSET: usize = 0x800;
/// Software-generated interrupt register.
const DISTRIBUTOR_SOFTWARE_INTERRUPT_OFFSET: usize = 0xf00;
/// Per-core GICv2 target masks; zero until that core initializes its interface.
static CORE_TARGET_MASKS: [AtomicU32; CORE_COUNT] = [const { AtomicU32::new(0) }; CORE_COUNT];

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

/// Software interrupt reserved for prompt scheduler dispatch.
pub const RESCHEDULE_INTERRUPT_IDENTIFIER: u32 = 1;

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

/// Prepares the calling core to receive timer and reschedule interrupts. Every core does this for itself.
pub fn initialise_core_interface() {
    // Match the timer's security group, retaining the existing platform policy.
    let groups = read_register(DISTRIBUTOR_BASE_ADDRESS + DISTRIBUTOR_GROUP_OFFSET);
    let reschedule_group =
        ((groups >> TIMER_INTERRUPT_IDENTIFIER) & 1) << RESCHEDULE_INTERRUPT_IDENTIFIER;
    write_register(
        DISTRIBUTOR_BASE_ADDRESS + DISTRIBUTOR_GROUP_OFFSET,
        (groups & !(1 << RESCHEDULE_INTERRUPT_IDENTIFIER)) | reschedule_group,
    );
    // SAFETY: This byte is the banked priority of our reserved SGI.
    unsafe {
        write_volatile(
            (DISTRIBUTOR_BASE_ADDRESS
                + DISTRIBUTOR_PRIORITY_OFFSET
                + RESCHEDULE_INTERRUPT_IDENTIFIER as usize) as *mut u8,
            TIMER_PRIORITY,
        );
    }
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
        (1 << TIMER_INTERRUPT_IDENTIFIER) | (1 << RESCHEDULE_INTERRUPT_IDENTIFIER),
    );
    write_register(
        CPU_INTERFACE_BASE_ADDRESS + CPU_INTERFACE_PRIORITY_MASK_OFFSET,
        ACCEPT_ALL_PRIORITIES,
    );
    write_register(
        CPU_INTERFACE_BASE_ADDRESS + CPU_INTERFACE_CONTROL_OFFSET,
        ENABLE,
    );
    if let Some(core_local) = current_core_local() {
        if let Some(target) = CORE_TARGET_MASKS.get(core_local.core_index()) {
            let target_mask =
                read_register(DISTRIBUTOR_BASE_ADDRESS + DISTRIBUTOR_TARGET_OFFSET) & 0xff;
            target.store(target_mask, Ordering::Release);
        }
    }
}

/// Requests dispatch on one initialized GICv2 CPU interface.
///
/// Called after publishing work, with local IRQs masked. Target masks come from
/// each interface's banked registers, not from an assumed MPIDR-to-bit mapping.
/// Before interface initialization, periodic scheduling remains the fallback.
pub fn request_reschedule(core_index: usize) {
    let Some(target) = CORE_TARGET_MASKS.get(core_index) else {
        return;
    };
    let target_mask = target.load(Ordering::Acquire);
    if target_mask == 0 {
        return;
    }
    // SAFETY: Complete published memory writes before the interrupt is generated.
    // Do not use nomem: this is also a compiler barrier.
    unsafe {
        core::arch::asm!("dsb ishst", options(nostack, preserves_flags));
    }
    write_register(
        DISTRIBUTOR_BASE_ADDRESS + DISTRIBUTOR_SOFTWARE_INTERRUPT_OFFSET,
        (target_mask << 16) | RESCHEDULE_INTERRUPT_IDENTIFIER,
    );
    // SAFETY: Complete the device write before leaving the notification path.
    unsafe {
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
    }
}

/// Takes the highest-priority pending interrupt, preserving its SGI source bits.
/// The complete token must be supplied to `end_of_interrupt` unchanged.
#[must_use]
pub fn acknowledge_interrupt() -> Option<u32> {
    let token = read_register(CPU_INTERFACE_BASE_ADDRESS + CPU_INTERFACE_ACKNOWLEDGE_OFFSET);
    (interrupt_identifier(token) < FIRST_SPURIOUS_IDENTIFIER).then_some(token)
}

/// Extracts the dispatch identifier without discarding the original EOI token.
#[must_use]
pub const fn interrupt_identifier(token: u32) -> u32 {
    token & INTERRUPT_IDENTIFIER_MASK
}

/// Completes the exact acknowledged interrupt, including the SGI source CPU.
pub fn end_of_interrupt(token: u32) {
    write_register(
        CPU_INTERFACE_BASE_ADDRESS + CPU_INTERFACE_END_OF_INTERRUPT_OFFSET,
        token,
    );
}
