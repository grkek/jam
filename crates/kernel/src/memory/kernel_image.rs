//! Where the kernel image sits in physical memory, as the linker placed it.

use super::address_translation::virtual_to_physical;

unsafe extern "C" {
    static __text_start: u8;
    static __kernel_end: u8;
}

/// Physical address where the kernel's code begins.
#[must_use]
pub fn kernel_image_start_address() -> usize {
    // SAFETY: `__text_start` is a symbol the linker script defines. Only its address is taken.
    virtual_to_physical(&raw const __text_start as usize)
}

/// Physical address after the kernel image, its stacks, and its padding.
#[must_use]
pub fn kernel_image_end_address() -> usize {
    // SAFETY: `__kernel_end` is a symbol the linker script defines. Only its address is taken.
    virtual_to_physical(&raw const __kernel_end as usize)
}
