//! Where the kernel image sits in physical memory, as the linker placed it.

unsafe extern "C" {
    static __text_start: u8;
    static __kernel_end: u8;
}

/// First address of the kernel image.
#[must_use]
pub fn kernel_image_start_address() -> usize {
    // SAFETY: `__text_start` is a symbol the linker script defines. Only its address is taken.
    &raw const __text_start as usize
}

/// First address after the kernel image, its stacks, and its padding.
#[must_use]
pub fn kernel_image_end_address() -> usize {
    // SAFETY: `__kernel_end` is a symbol the linker script defines. Only its address is taken.
    &raw const __kernel_end as usize
}
