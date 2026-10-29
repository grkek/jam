//! The kernel's own address space, in the upper half.
//!
//! Devices and all of RAM appear at `physical address + KERNEL_VIRTUAL_OFFSET`. Inside the kernel
//! image, code is read-only and executable, constants are read-only, and everything else is
//! writable and never executable. The lower half stays empty, ready for each actor's own space.

use foundation::{
    device_tree::DeviceTreeSummary,
    page_table_entry::{
        BLOCK_BYTE_COUNT, KernelAccess, MemoryKind, PAGE_BYTE_COUNT, PageAttributeError,
        PageAttributes,
    },
};

use super::{
    address_space::{AddressSpace, MapError},
    address_translation::{physical_to_virtual, virtual_to_physical},
    frame_allocator::FrameAllocator,
};
use crate::{
    architecture::aarch64::memory_management_unit::switch_kernel_translation_root,
    scheduling::core_local::CORE_MAXIMUM,
};

/// Start of the device region on QEMU `virt`: interrupt controller, UART, and friends.
const DEVICE_REGION_START_ADDRESS: usize = 0x0800_0000;
/// End of the device region mapped for now.
const DEVICE_REGION_END_ADDRESS: usize = 0x0a00_0000;

unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __interrupt_stacks_bottom: u8;
    static __interrupt_stacks_top: u8;
    static __reclamation_stacks_bottom: u8;
    static __reclamation_stacks_top: u8;
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

/// Builds the kernel address space and makes the calling core use it.
///
/// The boot code already turned translation on with coarse tables. This replaces them with
/// strict ones and switches off the lower half. The returned address space owns the translation
/// tables and must stay alive.
///
/// # Errors
///
/// Fails when the memory layout cannot be represented. Nothing has been switched in that case.

/// Builds the kernel address space and makes the calling core use it.
///
/// The boot code already enabled translation with coarse tables. This replaces
/// them with strict mappings, preserving kernel permissions and leaving stack
/// guard pages unmapped.
///
/// # Errors
///
/// Fails when the memory layout cannot be represented, the stack regions
/// are invalid, or a translation-table allocation or mapping fails.
pub fn enable_kernel_address_space(
    summary: &DeviceTreeSummary,
    frame_allocator: &mut FrameAllocator,
) -> Result<AddressSpace, KernelMapError> {
    let device = PageAttributes::new(MemoryKind::Device, KernelAccess::ReadWrite, false)?;
    let code = PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadOnly, true)?;
    let constants = PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadOnly, false)?;
    let data = PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadWrite, false)?;

    // SAFETY: All symbols are defined by the linker script. Only their addresses are taken.
    let (text_start, text_end, rodata_start, rodata_end) = (
        &raw const __text_start as usize,
        &raw const __text_end as usize,
        &raw const __rodata_start as usize,
        &raw const __rodata_end as usize,
    );

    let interrupt_bottom = virtual_to_physical(&raw const __interrupt_stacks_bottom as usize);
    let interrupt_top = virtual_to_physical(&raw const __interrupt_stacks_top as usize);

    let reclamation_bottom = virtual_to_physical(&raw const __reclamation_stacks_bottom as usize);
    let reclamation_top = virtual_to_physical(&raw const __reclamation_stacks_top as usize);

    let memory_start = usize::try_from(summary.memory_base_address)
        .map_err(|_| KernelMapError::MemoryRegionUnsupported)?;
    let memory_byte_count = usize::try_from(summary.memory_byte_count)
        .map_err(|_| KernelMapError::MemoryRegionUnsupported)?;
    let memory_end = memory_start
        .checked_add(memory_byte_count)
        .ok_or(KernelMapError::MemoryRegionUnsupported)?;

    let image_block_start = align_down(virtual_to_physical(text_start), BLOCK_BYTE_COUNT);
    let image_block_end = image_block_start + BLOCK_BYTE_COUNT;

    if virtual_to_physical(rodata_end) > image_block_end
        || virtual_to_physical(text_start) < memory_start
    {
        return Err(KernelMapError::ImageOutsideBlock);
    }

    // Each stack region contains CORE_MAXIMUM slices. The first page of
    // every slice is reserved as an unmapped guard page.
    let stack_regions = [
        (interrupt_bottom, interrupt_top),
        (reclamation_bottom, reclamation_top),
    ];

    let mut guard_regions = [(0usize, 0usize); 2];

    for (index, &(bottom, top)) in stack_regions.iter().enumerate() {
        let region_length = top
            .checked_sub(bottom)
            .ok_or(KernelMapError::MemoryRegionUnsupported)?;

        if bottom < memory_start
            || top > memory_end
            || bottom % PAGE_BYTE_COUNT != 0
            || region_length % CORE_MAXIMUM != 0
        {
            return Err(KernelMapError::MemoryRegionUnsupported);
        }

        let stride = region_length / CORE_MAXIMUM;

        if stride <= PAGE_BYTE_COUNT || stride % PAGE_BYTE_COUNT != 0 {
            return Err(KernelMapError::MemoryRegionUnsupported);
        }

        guard_regions[index] = (bottom, stride);
    }

    // Checks whether a physical page belongs to a stack guard.
    let is_guard_page = |page_address: usize| {
        guard_regions.iter().any(|&(bottom, stride)| {
            page_address >= bottom
                && page_address - bottom < CORE_MAXIMUM * stride
                && (page_address - bottom) % stride < PAGE_BYTE_COUNT
        })
    };

    // Blocks containing guard pages must use individual 4 KiB mappings.
    let block_contains_guard = |block_address: usize| {
        guard_regions.iter().any(|&(bottom, stride)| {
            (0..CORE_MAXIMUM).any(|core_index| {
                let guard_address = bottom + core_index * stride;

                guard_address >= block_address && guard_address < block_address + BLOCK_BYTE_COUNT
            })
        })
    };

    let mut address_space =
        AddressSpace::new(frame_allocator).ok_or(KernelMapError::AddressSpaceUnavailable)?;

    // Map MMIO devices using 2 MiB blocks.
    for physical_address in
        (DEVICE_REGION_START_ADDRESS..DEVICE_REGION_END_ADDRESS).step_by(BLOCK_BYTE_COUNT)
    {
        address_space.map_block(
            frame_allocator,
            physical_to_virtual(physical_address),
            physical_address,
            device,
        )?;
    }

    let first_block = align_down(memory_start, BLOCK_BYTE_COUNT);
    let last_block_end = align_up(memory_end, BLOCK_BYTE_COUNT);

    // Map ordinary RAM using 2 MiB blocks. The image block and any
    // blocks containing stack guards use 4 KiB page mappings instead.
    for physical_address in (first_block..last_block_end).step_by(BLOCK_BYTE_COUNT) {
        let requires_page_mappings =
            physical_address == image_block_start || block_contains_guard(physical_address);

        if !requires_page_mappings {
            address_space.map_block(
                frame_allocator,
                physical_to_virtual(physical_address),
                physical_address,
                data,
            )?;

            continue;
        }

        for page_address in
            (physical_address..physical_address + BLOCK_BYTE_COUNT).step_by(PAGE_BYTE_COUNT)
        {
            // Do not create a translation for stack guard pages.
            if is_guard_page(page_address) {
                continue;
            }

            let virtual_address = physical_to_virtual(page_address);

            let attributes = if (text_start..text_end).contains(&virtual_address) {
                code
            } else if (rodata_start..rodata_end).contains(&virtual_address) {
                constants
            } else {
                data
            };

            address_space.map_page(frame_allocator, virtual_address, page_address, attributes)?;
        }
    }

    // Verify every stack guard page has no translation.
    for &(bottom, stride) in &guard_regions {
        for core_index in 0..CORE_MAXIMUM {
            let guard_address = bottom + core_index * stride;

            assert!(
                address_space
                    .lookup_page(physical_to_virtual(guard_address))
                    .is_none(),
                "Stack guard page is unexpectedly mapped"
            );
        }
    }

    // Install the new kernel translation root with guard pages unmapped.
    switch_kernel_translation_root(address_space.root_table_address());

    Ok(address_space)
}
