//! Mapping memory into an actor's lower half for code at EL0 to use.

use core::ptr::{copy_nonoverlapping, write_bytes};

use foundation::page_table_entry::{
    PAGE_BYTE_COUNT, PageAttributeError, PageAttributes, UserAccess,
};

use super::{
    address_space::{AddressSpace, MapError},
    address_translation::physical_to_virtual,
    frame_allocator::FrameAllocator,
};
use crate::architecture::aarch64::cache_maintenance::make_code_visible;

/// Where the self-test maps its code page.
pub const TEST_CODE_ADDRESS: usize = 0x40_0000;
/// Where the self-test maps its two data pages.
pub const TEST_DATA_ADDRESS: usize = 0x80_0000;
/// An address the self-test leaves unmapped.
const TEST_UNMAPPED_ADDRESS: usize = 0xc0_0000;
const TEST_CODE_BYTES: &[u8] = b"JAM!";

/// Why a region could not be mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserMapError {
    /// The start is not page-aligned or the initial contents do not fit.
    InvalidRegion,
    /// The permissions were refused.
    Attributes(PageAttributeError),
    /// No free frame was left.
    OutOfFrames,
    /// A page could not be mapped.
    Mapping(MapError),
}

/// Backs `byte_count` bytes of the address space, starting at `virtual_address`, with fresh frames.
///
/// The frames are zeroed, then `initial_contents` is copied to the start. Executable regions are
/// made visible to instruction fetch. Frames already taken stay taken when a later page fails.
///
/// # Errors
///
/// Fails when the region is invalid, the permissions are refused, or memory runs out.
pub fn map_user_region(
    frame_allocator: &mut FrameAllocator,
    address_space: &mut AddressSpace,
    virtual_address: usize,
    byte_count: usize,
    access: UserAccess,
    executable: bool,
    initial_contents: &[u8],
) -> Result<(), UserMapError> {
    if virtual_address % PAGE_BYTE_COUNT != 0 || initial_contents.len() > byte_count {
        return Err(UserMapError::InvalidRegion);
    }
    let attributes = PageAttributes::user(access, executable).map_err(UserMapError::Attributes)?;

    let page_count = byte_count.div_ceil(PAGE_BYTE_COUNT);
    let mut initial_chunks = initial_contents.chunks(PAGE_BYTE_COUNT);
    for page_index in 0..page_count {
        let frame_address = frame_allocator
            .allocate_frame()
            .ok_or(UserMapError::OutOfFrames)?;
        let destination = physical_to_virtual(frame_address);
        let chunk = initial_chunks.next().unwrap_or(&[]);

        // SAFETY: The frame was just allocated, so the kernel owns all `PAGE_BYTE_COUNT` bytes and
        // nothing else references them. All of RAM is mapped writable at its direct-map address.
        // The chunk is at most one page long and does not overlap the new frame.
        unsafe {
            write_bytes(destination as *mut u8, 0, PAGE_BYTE_COUNT);
            copy_nonoverlapping(chunk.as_ptr(), destination as *mut u8, chunk.len());
        }
        if executable {
            make_code_visible(destination, PAGE_BYTE_COUNT);
        }

        address_space
            .map_page(
                frame_allocator,
                virtual_address + page_index * PAGE_BYTE_COUNT,
                frame_address,
                attributes,
            )
            .map_err(UserMapError::Mapping)?;
    }
    Ok(())
}

/// Maps user pages into a scratch address space and checks what the page tables say about them.
///
/// The scratch tables and frames are not returned. Returns the first thing that was wrong.
///
/// # Errors
///
/// Describes the first check that failed.
pub fn run_user_mapping_self_test(
    frame_allocator: &mut FrameAllocator,
) -> Result<(), &'static str> {
    let mut address_space =
        AddressSpace::new(frame_allocator).ok_or("no frame for the root table")?;

    map_user_region(
        frame_allocator,
        &mut address_space,
        TEST_CODE_ADDRESS,
        PAGE_BYTE_COUNT,
        UserAccess::ReadOnly,
        true,
        TEST_CODE_BYTES,
    )
    .map_err(|_| "mapping the code page failed")?;
    map_user_region(
        frame_allocator,
        &mut address_space,
        TEST_DATA_ADDRESS,
        2 * PAGE_BYTE_COUNT,
        UserAccess::ReadWrite,
        false,
        &[],
    )
    .map_err(|_| "mapping the data pages failed")?;

    let code = address_space
        .lookup_page(TEST_CODE_ADDRESS)
        .ok_or("the code page is not mapped")?;
    if !(code.is_user_accessible()
        && code.is_user_executable()
        && !code.is_privileged_executable()
        && code.is_read_only()
        && code.is_non_global())
    {
        return Err("the code page has the wrong permissions");
    }

    for page_index in 0..2 {
        let data = address_space
            .lookup_page(TEST_DATA_ADDRESS + page_index * PAGE_BYTE_COUNT)
            .ok_or("a data page is not mapped")?;
        if !(data.is_user_accessible()
            && !data.is_user_executable()
            && !data.is_privileged_executable()
            && !data.is_read_only()
            && data.is_non_global())
        {
            return Err("a data page has the wrong permissions");
        }
    }

    if address_space.lookup_page(TEST_UNMAPPED_ADDRESS).is_some() {
        return Err("an address that was never mapped is mapped");
    }

    let frame_address =
        usize::try_from(code.output_address()).map_err(|_| "the code frame is out of range")?;
    // SAFETY: The frame belongs to the scratch address space made above, which only this function
    // uses, and all of RAM is mapped readable at its direct-map address. The slice is shorter
    // than the page.
    let copied = unsafe {
        core::slice::from_raw_parts(
            physical_to_virtual(frame_address) as *const u8,
            TEST_CODE_BYTES.len(),
        )
    };
    if copied != TEST_CODE_BYTES {
        return Err("the initial contents were not copied");
    }
    Ok(())
}
