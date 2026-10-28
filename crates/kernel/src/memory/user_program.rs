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

/// Reads kernel memory from EL0, which the hardware must refuse.
///
///   0: d2800000  movz x0, #0
///   4: f2a80400  movk x0, #0x4020, lsl #16
///   8: f2dff000  movk x0, #0xff80, lsl #32
///   c: f2ffffe0  movk x0, #0xffff, lsl #48   (x0 = 0xffffff8040200000, in the kernel image)
///  10: f9400001  ldr  x1, [x0]
///  14: 14000000  b    .
pub const EL0_FAULT_PROGRAM: &[u8] = &[
    0x00, 0x00, 0x80, 0xd2, // movz x0, #0
    0x00, 0x04, 0xa8, 0xf2, // movk x0, #0x4020, lsl #16
    0x00, 0xf0, 0xdf, 0xf2, // movk x0, #0xff80, lsl #32
    0xe0, 0xff, 0xff, 0xf2, // movk x0, #0xffff, lsl #48
    0x01, 0x00, 0x40, 0xf9, // ldr  x1, [x0]
    0x00, 0x00, 0x00, 0x14, // b    .
];

/// Builds an address space with the private page, the given program as code, and one stack page.
pub fn build_user_address_space(
    frame_allocator: &mut FrameAllocator,
    private_initial_value: u64,
    program: &[u8],
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
        program,
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
