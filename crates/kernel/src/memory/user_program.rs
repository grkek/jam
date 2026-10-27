//! The first EL0 program: raw machine code embedded in the kernel.

use foundation::page_table_entry::{PAGE_BYTE_COUNT, UserAccess};

use super::{
    address_space::AddressSpace,
    frame_allocator::FrameAllocator,
    private_page,
    user_memory::{self, TEST_CODE_ADDRESS, TEST_DATA_ADDRESS, UserMapError},
};

/// CurrentAddress (x8=8), then FinishActor (x8=7), then spin if we ever return.
///
///   0: d2800108  movz x8, #8
///   4: d4000001  svc  #0
///   8: d28000e8  movz x8, #7
///   c: d4000001  svc  #0
///  10: 14000000  b    .
pub const EL0_SVC_LOOP: &[u8] = &[
    0x08, 0x01, 0x80, 0xd2, // movz x8, #8
    0x01, 0x00, 0x00, 0xd4, // svc  #0
    0xe8, 0x00, 0x80, 0xd2, // movz x8, #7
    0x01, 0x00, 0x00, 0xd4, // svc  #0
    0x00, 0x00, 0x00, 0x14, // b    .
];

/// Builds an address space with the private page, one executable code page, and one stack page.
pub fn build_user_address_space(
    frame_allocator: &mut FrameAllocator,
    private_initial_value: u64,
) -> Result<AddressSpace, UserMapError> {
    let mut address_space =
        private_page::build_address_space_with_private_page(frame_allocator, private_initial_value)
            .ok_or(UserMapError::OutOfFrames)?;

    user_memory::map_user_region(
        frame_allocator,
        &mut address_space,
        TEST_CODE_ADDRESS,
        PAGE_BYTE_COUNT,
        UserAccess::ReadOnly,
        true,
        EL0_SVC_LOOP,
    )?;

    user_memory::map_user_region(
        frame_allocator,
        &mut address_space,
        TEST_DATA_ADDRESS,
        PAGE_BYTE_COUNT,
        UserAccess::ReadWrite,
        false,
        &[],
    )?;

    Ok(address_space)
}

#[must_use]
/// :ditto:
pub const fn user_stack_top() -> usize {
    TEST_DATA_ADDRESS + PAGE_BYTE_COUNT
}
