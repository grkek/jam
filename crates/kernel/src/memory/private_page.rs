//! A private page for every actor, at the same virtual address in each actor's own space.
//!
//! Temporary scaffolding: it proves each actor really has its own lower half before any
//! code runs at EL0. The page is writable by the kernel only.

use core::ptr::{read_volatile, write_volatile};

use foundation::page_table_entry::{KernelAccess, MemoryKind, PageAttributes};

use super::{
    address_space::AddressSpace, address_translation::physical_to_virtual,
    frame_allocator::FrameAllocator,
};

/// Where every actor sees its own private page.
pub const PRIVATE_PAGE_VIRTUAL_ADDRESS: usize = 0x1000_0000;
/// Spacing between the initial values of neighbouring actors' pages.
pub const PRIVATE_VALUE_STRIDE: u64 = 1000;

/// Builds a lower-half address space holding one private page that starts out as `initial_value`.
pub fn build_address_space_with_private_page(
    frame_allocator: &mut FrameAllocator,
    initial_value: u64,
) -> Option<AddressSpace> {
    let attributes =
        PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadWrite, false).ok()?;
    let mut address_space = AddressSpace::new(frame_allocator)?;
    let frame_address = frame_allocator.allocate_frame()?;

    // SAFETY: The frame was just allocated, so the kernel owns it, and all of RAM is mapped
    // writable at its direct-map address. The address is frame-aligned, so aligned for `u64`.
    unsafe {
        write_volatile(
            physical_to_virtual(frame_address) as *mut u64,
            initial_value,
        );
    }

    address_space
        .map_page(
            frame_allocator,
            PRIVATE_PAGE_VIRTUAL_ADDRESS,
            frame_address,
            attributes,
        )
        .ok()?;
    Some(address_space)
}

/// Reads the calling actor's private value. Only actors spawned with a private page may call this.
pub fn read_private_value() -> u64 {
    // SAFETY: Spawning maps a private page at this address for every actor, and the actor's own
    // address space is active while it runs. The page is kernel-readable and 8-byte aligned.
    unsafe { read_volatile(PRIVATE_PAGE_VIRTUAL_ADDRESS as *const u64) }
}

/// Writes the calling actor's private value. Only actors spawned with a private page may call this.
pub fn write_private_value(value: u64) {
    // SAFETY: As in `read_private_value`; the page is kernel-writable.
    unsafe { write_volatile(PRIVATE_PAGE_VIRTUAL_ADDRESS as *mut u64, value) }
}
