//! The kernel's upper-half address space and per-core stack guards.
//!
//! The kernel maps RAM and devices at their physical addresses plus
//! `KERNEL_VIRTUAL_OFFSET`. Stack guard pages are deliberately left unmapped.
//! Code is read-only and executable; constants are read-only and non-executable.
//! All remaining kernel data is writable and non-executable.

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

/// Every stack slice starts with one unmapped 4 KiB page.
const STACK_GUARD_BYTE_COUNT: usize = PAGE_BYTE_COUNT;
/// Boot, idle, interrupt, and reclamation stack families.
const STACK_REGION_COUNT: usize = 4;

unsafe extern "C" {
    /// First virtual address of executable kernel code.
    static __text_start: u8;
    /// Exclusive virtual end of executable kernel code.
    static __text_end: u8;
    /// First virtual address of read-only kernel constants.
    static __rodata_start: u8;
    /// Exclusive virtual end of read-only kernel constants.
    static __rodata_end: u8;

    /// First virtual address of the boot-stack allocation, including guards.
    static __boot_stacks_bottom: u8;
    /// Exclusive virtual end of the boot-stack allocation.
    static __boot_stacks_top: u8;
    /// First virtual address of the idle-stack allocation, including guards.
    static __idle_stacks_bottom: u8;
    /// Exclusive virtual end of the idle-stack allocation.
    static __idle_stacks_top: u8;
    /// First virtual address of the interrupt-stack allocation, including guards.
    static __interrupt_stacks_bottom: u8;
    /// Exclusive virtual end of the interrupt-stack allocation.
    static __interrupt_stacks_top: u8;
    /// First virtual address of the reclamation-stack allocation, including guards.
    static __reclamation_stacks_bottom: u8;
    /// Exclusive virtual end of the reclamation-stack allocation.
    static __reclamation_stacks_top: u8;
}

/// Why construction of the kernel's address space failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelMapError {
    /// A root page-table frame could not be allocated.
    AddressSpaceUnavailable,
    /// A RAM or stack region has an invalid size, address, or alignment.
    MemoryRegionUnsupported,
    /// The protected kernel image cannot fit in the supported image block.
    ImageOutsideBlock,
    /// The requested page attributes are invalid.
    Attributes(PageAttributeError),
    /// A translation could not be installed.
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

// Rounds an address down to a page or block boundary.
fn align_down(address: usize, alignment: usize) -> usize {
    address & !(alignment - 1)
}

// Rounds an address up to a page or block boundary.
fn align_up(address: usize, alignment: usize) -> usize {
    align_down(address + alignment - 1, alignment)
}

/// Replaces the coarse boot mappings with protected kernel translations.
///
/// Every stack family contains `CORE_MAXIMUM` slices. The first page in each
/// slice is excluded from the page tables, so a downward-growing stack faults
/// before it can continue into the preceding allocation.
///
/// # Errors
///
/// Returns an error if the memory layout or any page-table mapping is invalid.
/// The calling core does not switch to the new translation root on failure.
pub fn enable_kernel_address_space(
    summary: &DeviceTreeSummary,
    frame_allocator: &mut FrameAllocator,
) -> Result<AddressSpace, KernelMapError> {
    // Device registers, executable code, constants, and writable data have distinct permissions.
    let device = PageAttributes::new(MemoryKind::Device, KernelAccess::ReadWrite, false)?;
    let code = PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadOnly, true)?;
    let constants = PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadOnly, false)?;
    let data = PageAttributes::new(MemoryKind::Normal, KernelAccess::ReadWrite, false)?;

    // The linker defines these virtual boundaries; only their addresses are read.
    let (text_start, text_end, rodata_start, rodata_end) = (
        &raw const __text_start as usize,
        &raw const __text_end as usize,
        &raw const __rodata_start as usize,
        &raw const __rodata_end as usize,
    );

    // Convert linker-defined upper-half stack addresses to physical RAM addresses.
    let boot_bottom = virtual_to_physical(&raw const __boot_stacks_bottom as usize);
    let boot_top = virtual_to_physical(&raw const __boot_stacks_top as usize);
    let idle_bottom = virtual_to_physical(&raw const __idle_stacks_bottom as usize);
    let idle_top = virtual_to_physical(&raw const __idle_stacks_top as usize);
    let interrupt_bottom = virtual_to_physical(&raw const __interrupt_stacks_bottom as usize);
    let interrupt_top = virtual_to_physical(&raw const __interrupt_stacks_top as usize);
    let reclamation_bottom = virtual_to_physical(&raw const __reclamation_stacks_bottom as usize);
    let reclamation_top = virtual_to_physical(&raw const __reclamation_stacks_top as usize);

    // Validate the RAM interval reported by the device tree.
    let memory_start = usize::try_from(summary.memory_base_address)
        .map_err(|_| KernelMapError::MemoryRegionUnsupported)?;
    let memory_byte_count = usize::try_from(summary.memory_byte_count)
        .map_err(|_| KernelMapError::MemoryRegionUnsupported)?;
    let memory_end = memory_start
        .checked_add(memory_byte_count)
        .ok_or(KernelMapError::MemoryRegionUnsupported)?;

    // The executable and read-only image sections currently share one 2 MiB block.
    let image_block_start = align_down(virtual_to_physical(text_start), BLOCK_BYTE_COUNT);
    let image_block_end = image_block_start + BLOCK_BYTE_COUNT;

    if virtual_to_physical(rodata_end) > image_block_end
        || virtual_to_physical(text_start) < memory_start
    {
        return Err(KernelMapError::ImageOutsideBlock);
    }

    // Each entry is a physical interval containing one slice per execution core.
    let stack_regions = [
        (boot_bottom, boot_top),
        (idle_bottom, idle_top),
        (interrupt_bottom, interrupt_top),
        (reclamation_bottom, reclamation_top),
    ];

    // Each guard region records its first physical address and per-core stride.
    let mut guard_regions = [(0usize, 0usize); STACK_REGION_COUNT];

    for (index, &(bottom, top)) in stack_regions.iter().enumerate() {
        let region_length = top
            .checked_sub(bottom)
            .ok_or(KernelMapError::MemoryRegionUnsupported)?;

        // All stack regions must fit in RAM and divide evenly among the cores.
        if bottom < memory_start
            || top > memory_end
            || bottom % PAGE_BYTE_COUNT != 0
            || region_length % CORE_MAXIMUM != 0
        {
            return Err(KernelMapError::MemoryRegionUnsupported);
        }

        let stride = region_length / CORE_MAXIMUM;

        // A slice must contain at least one guard page and one usable page.
        if stride <= STACK_GUARD_BYTE_COUNT || stride % PAGE_BYTE_COUNT != 0 {
            return Err(KernelMapError::MemoryRegionUnsupported);
        }

        guard_regions[index] = (bottom, stride);
    }

    // Recognize the first unmapped page of each per-core stack slice.
    let is_guard_page = |page_address: usize| {
        guard_regions.iter().any(|&(bottom, stride)| {
            page_address >= bottom
                && page_address - bottom < CORE_MAXIMUM * stride
                && (page_address - bottom) % stride < STACK_GUARD_BYTE_COUNT
        })
    };

    // Fall back to 4 KiB mappings for any 2 MiB block containing a guard.
    let block_contains_guard = |block_address: usize| {
        guard_regions.iter().any(|&(bottom, stride)| {
            (0..CORE_MAXIMUM).any(|core_index| {
                let guard_address = bottom + core_index * stride;
                guard_address >= block_address && guard_address < block_address + BLOCK_BYTE_COUNT
            })
        })
    };

    // Allocate a new translation root without changing the active boot mappings yet.
    let mut address_space =
        AddressSpace::new(frame_allocator).ok_or(KernelMapError::AddressSpaceUnavailable)?;

    // Map the MMIO window with device-memory attributes.
    // Map only the device intervals named by the selected platform.
    // This first portability phase preserves 2 MiB block mappings.
    for region in crate::platform::current::EARLY_DEVICE_REGIONS {
        // Reject invalid, misaligned, or RAM-overlapping MMIO regions.
        if region.start_address >= region.end_address
            || region.start_address % BLOCK_BYTE_COUNT != 0
            || region.end_address % BLOCK_BYTE_COUNT != 0
            || (region.start_address < memory_end && region.end_address > memory_start)
        {
            return Err(KernelMapError::MemoryRegionUnsupported);
        }

        for physical_address in (region.start_address..region.end_address).step_by(BLOCK_BYTE_COUNT)
        {
            address_space.map_block(
                frame_allocator,
                physical_to_virtual(physical_address),
                physical_address,
                device,
            )?;
        }
    }

    let first_block = align_down(memory_start, BLOCK_BYTE_COUNT);
    let last_block_end = align_up(memory_end, BLOCK_BYTE_COUNT);

    // Use page mappings for protected image sections and blocks containing guards.
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
            // Guard pages are intentionally absent from the translation tables.
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

    // Inspect all four stack families to ensure no guard acquired a translation.
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

    // Install the protected kernel translations only after all mappings are valid.
    switch_kernel_translation_root(address_space.root_table_address());

    Ok(address_space)
}
