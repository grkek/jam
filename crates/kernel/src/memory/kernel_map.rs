//! The kernel's own address space: an identity map with strict permissions.
//!
//! Devices and RAM are mapped one-to-one. Inside the kernel image, code is read-only and
//! executable, constants are read-only, and everything else is writable and never executable.
//! This is a stepping stone: once actors run at EL0, the kernel moves to the upper half
//! (`TTBR1_EL1`) so each actor can own `TTBR0_EL1`.

use foundation::{
    device_tree::DeviceTreeSummary,
    page_table_entry::{
        BLOCK_BYTE_COUNT, KernelAccess, MemoryKind, PAGE_BYTE_COUNT, PageAttributeError,
        PageAttributes,
    },
};

use super::{
    address_space::{AddressSpace, MapError},
    frame_allocator::FrameAllocator,
};
use crate::architecture::aarch64::memory_management_unit::switch_on_memory_management_unit;

/// Start of the device region on QEMU `virt`: interrupt controller, UART, and friends.
const DEVICE_REGION_START_ADDRESS: usize = 0x0800_0000;
/// End of the device region mapped for now.
const DEVICE_REGION_END_ADDRESS: usize = 0x0a00_0000;

unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
}

/// Why the kernel address space could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelMapError {
    /// The root translation table could not be allocated.
    AddressSpaceUnavailable,
    /// The memory region from the device tree does not fit in this address space.
    MemoryRegionUnsupported,
    /// The kernel's code and constants do not fit in one 2 MiB block.
    ImageOutsideBlock,
    /// An attribute combination was refused.
    Attributes(PageAttributeError),
    /// A mapping failed.
    Mapping(MapError),
}

impl From<PageAttributeError> for KernelMapError {
    fn from(error: PageAttributeError) -> Self {
        Self::Attributes(error)
    }
}

impl From<MapError> for KernelMapError {
    fn from(error: MapError) -> Self {
        Self::Mapping(error)
    }
}

fn align_down(address: usize, alignment: usize) -> usize {
    address & !(alignment - 1)
}

fn align_up(address: usize, alignment: usize) -> usize {
    align_down(address + alignment - 1, alignment)
}

/// Builds the kernel address space and switches the memory management unit on.
///
/// The returned address space owns the translation tables and must stay alive.
///
/// # Errors
///
/// Fails when the memory layout cannot be represented. Nothing has been switched on in that case.
pub fn enable_kernel_address_space(
    summary: &DeviceTreeSummary,
    frame_allocator: &mut FrameAllocator,
) -> Result<AddressSpace, KernelMapError> {
    let device = PageAttributes::new(MemoryKind::Device, KernelAccess::ReadWrite, false)?;
    let code = PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadOnly, true)?;
    let constants = PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadOnly, false)?;
    let data = PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadWrite, false)?;

    // SAFETY: These symbols come from the linker script. Only their addresses are taken.
    let (text_start, text_end, rodata_start, rodata_end) = (
        &raw const __text_start as usize,
        &raw const __text_end as usize,
        &raw const __rodata_start as usize,
        &raw const __rodata_end as usize,
    );

    let memory_start = usize::try_from(summary.memory_base_address)
        .map_err(|_| KernelMapError::MemoryRegionUnsupported)?;
    let memory_byte_count = usize::try_from(summary.memory_byte_count)
        .map_err(|_| KernelMapError::MemoryRegionUnsupported)?;
    let memory_end = memory_start + memory_byte_count;

    let image_block_start = align_down(text_start, BLOCK_BYTE_COUNT);
    let image_block_end = image_block_start + BLOCK_BYTE_COUNT;
    if rodata_end > image_block_end || text_start < memory_start {
        return Err(KernelMapError::ImageOutsideBlock);
    }

    let mut address_space =
        AddressSpace::new(frame_allocator).ok_or(KernelMapError::AddressSpaceUnavailable)?;

    for block_address in
        (DEVICE_REGION_START_ADDRESS..DEVICE_REGION_END_ADDRESS).step_by(BLOCK_BYTE_COUNT)
    {
        address_space.map_block(frame_allocator, block_address, block_address, device)?;
    }

    let first_block = align_down(memory_start, BLOCK_BYTE_COUNT);
    let last_block_end = align_up(memory_end, BLOCK_BYTE_COUNT);
    for block_address in (first_block..last_block_end).step_by(BLOCK_BYTE_COUNT) {
        if block_address != image_block_start {
            address_space.map_block(frame_allocator, block_address, block_address, data)?;
        }
    }

    for page_address in (image_block_start..image_block_end).step_by(PAGE_BYTE_COUNT) {
        let attributes = if (text_start..text_end).contains(&page_address) {
            code
        } else if (rodata_start..rodata_end).contains(&page_address) {
            constants
        } else {
            data
        };
        address_space.map_page(frame_allocator, page_address, page_address, attributes)?;
    }

    switch_on_memory_management_unit(address_space.root_table_address());
    Ok(address_space)
}
