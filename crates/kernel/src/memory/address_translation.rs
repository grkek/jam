//! Converting between physical addresses and the kernel's upper-half addresses.
//!
//! All of RAM and the devices are mapped at `physical address + KERNEL_VIRTUAL_OFFSET`, and the
//! kernel image is linked there too, so one offset converts both.

/// Where physical address zero appears in the upper half. Must match `KERNEL_OFFSET` in the
/// linker script and the 39-bit address size set in `TCR_EL1`.
pub const KERNEL_VIRTUAL_OFFSET: usize = 0xFFFF_FF80_0000_0000;

/// The upper-half address through which the kernel reaches a physical address.
#[must_use]
pub const fn physical_to_virtual(physical_address: usize) -> usize {
    physical_address.wrapping_add(KERNEL_VIRTUAL_OFFSET)
}

/// The physical address behind an upper-half address of RAM, a device, or the kernel image.
#[must_use]
pub const fn virtual_to_physical(virtual_address: usize) -> usize {
    virtual_address.wrapping_sub(KERNEL_VIRTUAL_OFFSET)
}
