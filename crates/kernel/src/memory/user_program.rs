//! Loads static AArch64 ELF64 executables into a JAM actor address space.
//!
//! Requires a 39-bit TTBR0 user virtual-address range and 4 KiB pages.
//! This loader does not support PIE, dynamic linking, or ELF relocations.
//! The ELF fields are decoded byte by byte, so no unsafe code and no alignment assumptions.

use core::cmp;

use foundation::page_table_entry::{PAGE_BYTE_COUNT, UserAccess};

use super::{
    address_space::AddressSpace,
    frame_allocator::FrameAllocator,
    private_page,
    user_memory::{self, TEST_CODE_ADDRESS, TEST_DATA_ADDRESS, UserMapError},
};

const ELF_MAGIC_BYTES: &[u8] = &[0x7f, b'E', b'L', b'F'];
const ELF_CLASS_INDEX: usize = 4;
const ELF_DATA_INDEX: usize = 5;
const ELF_VERSION_INDEX: usize = 6;

const PAGE_SIZE: u64 = PAGE_BYTE_COUNT as u64;
const USER_VIRTUAL_ADDRESS_LIMIT: u64 = 1 << 39;
const ELF_USER_STACK_VIRTUAL_ADDRESS: usize = 0x0000_007f_ffff_0000;

const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LITTLE_ENDIAN: u8 = 1;
const ELF_CURRENT_VERSION: u8 = 1;
const ELF_TYPE_EXECUTABLE: u16 = 2;
const ELF_MACHINE_AARCH64: u16 = 183;
const ELF_VERSION_CURRENT: u32 = 1;

const ELF_HEADER_BYTE_COUNT: usize = 64;
const PROGRAM_HEADER_BYTE_COUNT: usize = 56;

const PROGRAM_HEADER_TYPE_LOAD: u32 = 1;
const PROGRAM_HEADER_TYPE_DYNAMIC: u32 = 2;
const PROGRAM_HEADER_TYPE_INTERPRETER: u32 = 3;
const PROGRAM_FLAG_EXECUTE: u32 = 1;
const PROGRAM_FLAG_WRITE: u32 = 2;

/// CurrentAddress (x8=8), then FinishActor (x8=7), then spin if we ever return.
pub const EL0_SVC_LOOP: &[u8] = &[
    0x08, 0x01, 0x80, 0xd2, // movz x8, #8
    0x01, 0x00, 0x00, 0xd4, // svc  #0
    0xe8, 0x00, 0x80, 0xd2, // movz x8, #7
    0x01, 0x00, 0x00, 0xd4, // svc  #0
    0x00, 0x00, 0x00, 0x14, // b    .
];

/// An EL0 program that deliberately attempts to read kernel memory.
pub const EL0_FAULT_PROGRAM: &[u8] = &[
    0x00, 0x00, 0x80, 0xd2, // movz x0, #0
    0x00, 0x04, 0xa8, 0xf2, // movk x0, #0x4020, lsl #16
    0x00, 0xf0, 0xdf, 0xf2, // movk x0, #0xff80, lsl #32
    0xe0, 0xff, 0xff, 0xf2, // movk x0, #0xffff, lsl #48
    0x01, 0x00, 0x40, 0xf9, // ldr  x1, [x0]
    0x00, 0x00, 0x00, 0x14, // b    .
];

/// The embedded, statically linked Genesis service.
pub const EL0_GENESIS_PROGRAM: &[u8] = include_bytes!(env!("GENESIS_ELF_PATH"));

/// The fields of the ELF header this loader uses.
struct ElfHeader {
    entry_point: u64,
    program_header_offset: u64,
    program_header_count: u16,
}

/// The fields of a program header this loader uses.
struct ProgramHeader {
    segment_type: u32,
    flags: u32,
    file_offset: u64,
    virtual_address: u64,
    file_size: u64,
    memory_size: u64,
    alignment: u64,
}

/// Converts a malformed ELF or unsupported mapping request into `InvalidRegion`.
fn invalid_region() -> UserMapError {
    UserMapError::InvalidRegion
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    bytes.get(offset..end)?.try_into().ok()
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, UserMapError> {
    read_array(bytes, offset)
        .map(u16::from_le_bytes)
        .ok_or_else(invalid_region)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, UserMapError> {
    read_array(bytes, offset)
        .map(u32::from_le_bytes)
        .ok_or_else(invalid_region)
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, UserMapError> {
    read_array(bytes, offset)
        .map(u64::from_le_bytes)
        .ok_or_else(invalid_region)
}

fn to_usize(value: u64) -> Result<usize, UserMapError> {
    usize::try_from(value).map_err(|_| invalid_region())
}

fn page_floor(address: u64) -> u64 {
    address & !(PAGE_SIZE - 1)
}

fn page_ceiling(address: u64) -> Result<u64, UserMapError> {
    address
        .checked_add(PAGE_SIZE - 1)
        .map(page_floor)
        .ok_or_else(invalid_region)
}

fn intervals_overlap(start_a: u64, end_a: u64, start_b: u64, end_b: u64) -> bool {
    start_a < end_b && start_b < end_a
}

/// Reads and checks the ELF header. Everything it returns has been range-checked.
fn parse_header(program: &[u8]) -> Result<ElfHeader, UserMapError> {
    let identification: [u8; 16] = read_array(program, 0).ok_or_else(invalid_region)?;
    let program_header_count = read_u16(program, 56)?;

    if !identification.starts_with(ELF_MAGIC_BYTES)
        || identification.get(ELF_CLASS_INDEX) != Some(&ELF_CLASS_64)
        || identification.get(ELF_DATA_INDEX) != Some(&ELF_DATA_LITTLE_ENDIAN)
        || identification.get(ELF_VERSION_INDEX) != Some(&ELF_CURRENT_VERSION)
        || read_u16(program, 16)? != ELF_TYPE_EXECUTABLE
        || read_u16(program, 18)? != ELF_MACHINE_AARCH64
        || read_u32(program, 20)? != ELF_VERSION_CURRENT
        || usize::from(read_u16(program, 52)?) != ELF_HEADER_BYTE_COUNT
        || usize::from(read_u16(program, 54)?) != PROGRAM_HEADER_BYTE_COUNT
        || program_header_count == 0
    {
        return Err(invalid_region());
    }

    Ok(ElfHeader {
        entry_point: read_u64(program, 24)?,
        program_header_offset: read_u64(program, 32)?,
        program_header_count,
    })
}

/// Reads one program header. The header table must already have been checked against the file size.
fn read_program_header(
    program: &[u8],
    header: &ElfHeader,
    index: usize,
) -> Result<ProgramHeader, UserMapError> {
    let table_offset = to_usize(header.program_header_offset)?;
    let offset = index
        .checked_mul(PROGRAM_HEADER_BYTE_COUNT)
        .and_then(|entry_offset| table_offset.checked_add(entry_offset))
        .ok_or_else(invalid_region)?;
    let entry = program
        .get(
            offset
                ..offset
                    .checked_add(PROGRAM_HEADER_BYTE_COUNT)
                    .ok_or_else(invalid_region)?,
        )
        .ok_or_else(invalid_region)?;

    Ok(ProgramHeader {
        segment_type: read_u32(entry, 0)?,
        flags: read_u32(entry, 4)?,
        file_offset: read_u64(entry, 8)?,
        virtual_address: read_u64(entry, 16)?,
        file_size: read_u64(entry, 32)?,
        memory_size: read_u64(entry, 40)?,
        alignment: read_u64(entry, 48)?,
    })
}

/// Validates the entire program *before* allocating user frames.
fn validate_elf(program: &[u8]) -> Result<ElfHeader, UserMapError> {
    let header = parse_header(program)?;

    let table_offset = to_usize(header.program_header_offset)?;
    let table_byte_count = usize::from(header.program_header_count)
        .checked_mul(PROGRAM_HEADER_BYTE_COUNT)
        .ok_or_else(invalid_region)?;
    let table_end = table_offset
        .checked_add(table_byte_count)
        .ok_or_else(invalid_region)?;
    if table_end > program.len() {
        return Err(invalid_region());
    }

    let entry_point = header.entry_point;
    if entry_point < PAGE_SIZE || entry_point % 4 != 0 || entry_point >= USER_VIRTUAL_ADDRESS_LIMIT
    {
        return Err(invalid_region());
    }

    let stack_start = ELF_USER_STACK_VIRTUAL_ADDRESS as u64;
    let stack_end = stack_start + PAGE_SIZE;
    let mut found_loadable_segment = false;
    let mut entry_is_file_backed_executable = false;

    for index in 0..usize::from(header.program_header_count) {
        let segment = read_program_header(program, &header, index)?;
        if segment.segment_type == PROGRAM_HEADER_TYPE_INTERPRETER
            || segment.segment_type == PROGRAM_HEADER_TYPE_DYNAMIC
        {
            // JAM does not provide a dynamic linker or relocation runtime yet.
            return Err(invalid_region());
        }
        if segment.segment_type != PROGRAM_HEADER_TYPE_LOAD {
            continue;
        }
        if segment.file_size > segment.memory_size {
            return Err(invalid_region());
        }

        let file_start = to_usize(segment.file_offset)?;
        let file_end = file_start
            .checked_add(to_usize(segment.file_size)?)
            .ok_or_else(invalid_region)?;
        if file_end > program.len() {
            return Err(invalid_region());
        }

        if segment.alignment > 1
            && (!segment.alignment.is_power_of_two()
                || segment.virtual_address % segment.alignment
                    != segment.file_offset % segment.alignment)
        {
            return Err(invalid_region());
        }
        // The file and virtual offsets must also agree modulo the page size.
        if segment.virtual_address % PAGE_SIZE != segment.file_offset % PAGE_SIZE {
            return Err(invalid_region());
        }

        let writable = segment.flags & PROGRAM_FLAG_WRITE != 0;
        let executable = segment.flags & PROGRAM_FLAG_EXECUTE != 0;
        if writable && executable {
            return Err(invalid_region()); // write-xor-execute
        }

        if segment.memory_size == 0 {
            continue;
        }
        found_loadable_segment = true;
        let segment_end = segment
            .virtual_address
            .checked_add(segment.memory_size)
            .ok_or_else(invalid_region)?;
        if segment.virtual_address < PAGE_SIZE
            || segment_end > USER_VIRTUAL_ADDRESS_LIMIT
            || intervals_overlap(segment.virtual_address, segment_end, stack_start, stack_end)
        {
            return Err(invalid_region());
        }

        // Distinct segments may share a page, but their byte ranges must not overlap.
        for prior_index in 0..index {
            let prior = read_program_header(program, &header, prior_index)?;
            if prior.segment_type != PROGRAM_HEADER_TYPE_LOAD || prior.memory_size == 0 {
                continue;
            }
            let prior_end = prior
                .virtual_address
                .checked_add(prior.memory_size)
                .ok_or_else(invalid_region)?;
            if intervals_overlap(
                segment.virtual_address,
                segment_end,
                prior.virtual_address,
                prior_end,
            ) {
                return Err(invalid_region());
            }
        }

        let file_virtual_end = segment
            .virtual_address
            .checked_add(segment.file_size)
            .ok_or_else(invalid_region)?;
        if executable
            && entry_point >= segment.virtual_address
            && entry_point
                .checked_add(4)
                .is_some_and(|end| end <= file_virtual_end)
        {
            entry_is_file_backed_executable = true;
        }
    }

    if !found_loadable_segment || !entry_is_file_backed_executable {
        return Err(invalid_region());
    }
    Ok(header)
}

/// Returns whether an earlier loadable segment already covers this page.
fn page_was_visited(
    program: &[u8],
    header: &ElfHeader,
    current_segment_index: usize,
    page_address: u64,
) -> Result<bool, UserMapError> {
    for prior_index in 0..current_segment_index {
        let prior = read_program_header(program, header, prior_index)?;
        if prior.segment_type != PROGRAM_HEADER_TYPE_LOAD || prior.memory_size == 0 {
            continue;
        }
        let prior_end = prior
            .virtual_address
            .checked_add(prior.memory_size)
            .ok_or_else(invalid_region)?;
        if page_floor(prior.virtual_address) <= page_address
            && page_address < page_ceiling(prior_end)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Composes one complete page from every loadable segment that touches it.
///
/// Returns the access the page needs. A page shared by a writable and an executable segment is
/// refused, because it could not be both under write-xor-execute.
fn build_segment_page(
    program: &[u8],
    header: &ElfHeader,
    page_address: u64,
    page_bytes: &mut [u8; PAGE_BYTE_COUNT],
) -> Result<(UserAccess, bool), UserMapError> {
    let page_end = page_address + PAGE_SIZE;
    let mut writable = false;
    let mut executable = false;
    page_bytes.fill(0);

    for index in 0..usize::from(header.program_header_count) {
        let segment = read_program_header(program, header, index)?;
        if segment.segment_type != PROGRAM_HEADER_TYPE_LOAD || segment.memory_size == 0 {
            continue;
        }
        let segment_end = segment.virtual_address + segment.memory_size; // range-checked by validate_elf
        if !intervals_overlap(page_address, page_end, segment.virtual_address, segment_end) {
            continue;
        }

        writable |= segment.flags & PROGRAM_FLAG_WRITE != 0;
        executable |= segment.flags & PROGRAM_FLAG_EXECUTE != 0;
        if writable && executable {
            return Err(invalid_region());
        }

        let file_end = segment.virtual_address + segment.file_size; // range-checked by validate_elf
        let copy_start = cmp::max(page_address, segment.virtual_address);
        let copy_end = cmp::min(page_end, file_end);
        if copy_start >= copy_end {
            continue; // zero-initialised data or an unused part of the page
        }

        let destination_offset = to_usize(copy_start - page_address)?;
        let copy_length = to_usize(copy_end - copy_start)?;
        let source_offset = to_usize(segment.file_offset + (copy_start - segment.virtual_address))?;
        let destination = page_bytes
            .get_mut(destination_offset..destination_offset + copy_length)
            .ok_or_else(invalid_region)?;
        let source = program
            .get(source_offset..source_offset + copy_length)
            .ok_or_else(invalid_region)?;
        destination.copy_from_slice(source);
    }

    let access = if writable {
        UserAccess::ReadWrite
    } else {
        UserAccess::ReadOnly
    };
    Ok((access, executable))
}

/// Maps every loadable page once, then the stack page.
fn map_segments_and_stack(
    frame_allocator: &mut FrameAllocator,
    address_space: &mut AddressSpace,
    program: &[u8],
    header: &ElfHeader,
) -> Result<(), UserMapError> {
    for index in 0..usize::from(header.program_header_count) {
        let segment = read_program_header(program, header, index)?;
        if segment.segment_type != PROGRAM_HEADER_TYPE_LOAD || segment.memory_size == 0 {
            continue;
        }

        let segment_end = segment.virtual_address + segment.memory_size; // range-checked by validate_elf
        let end_page = page_ceiling(segment_end)?;
        let mut page_address = page_floor(segment.virtual_address);
        while page_address < end_page {
            if !page_was_visited(program, header, index, page_address)? {
                let mut page_bytes = [0_u8; PAGE_BYTE_COUNT];
                let (access, executable) =
                    build_segment_page(program, header, page_address, &mut page_bytes)?;
                user_memory::map_user_region(
                    frame_allocator,
                    address_space,
                    to_usize(page_address)?,
                    PAGE_BYTE_COUNT,
                    access,
                    executable,
                    &page_bytes,
                )?;
            }
            page_address += PAGE_SIZE;
        }
    }

    user_memory::map_user_region(
        frame_allocator,
        address_space,
        ELF_USER_STACK_VIRTUAL_ADDRESS,
        PAGE_BYTE_COUNT,
        UserAccess::ReadWrite,
        false,
        &[],
    )
}

/// Validates and maps a statically linked AArch64 ELF64 executable and a one-page user stack.
///
/// Returns the address space and the entry point. When anything fails after the address space
/// exists, everything already taken is given back before the error is returned.
///
/// # Errors
///
/// Fails on a malformed or unsupported image, or when memory runs out.
pub fn build_elf_user_address_space(
    frame_allocator: &mut FrameAllocator,
    private_initial_value: u64,
    program: &[u8],
) -> Result<(AddressSpace, u64), UserMapError> {
    let header = validate_elf(program)?;

    // Fail early on a clearly oversized image. Page tables need frames too, so this is a floor.
    let mut requested_page_count = 2_u64; // the private page and the stack
    for index in 0..usize::from(header.program_header_count) {
        let segment = read_program_header(program, &header, index)?;
        if segment.segment_type != PROGRAM_HEADER_TYPE_LOAD || segment.memory_size == 0 {
            continue;
        }
        let start = page_floor(segment.virtual_address);
        let end = page_ceiling(segment.virtual_address + segment.memory_size)?;
        requested_page_count = requested_page_count
            .checked_add((end - start) / PAGE_SIZE)
            .ok_or_else(invalid_region)?;
    }
    if requested_page_count >= frame_allocator.free_frame_count() as u64 {
        return Err(UserMapError::OutOfFrames);
    }

    let mut address_space =
        private_page::build_address_space_with_private_page(frame_allocator, private_initial_value)
            .ok_or(UserMapError::OutOfFrames)?;

    match map_segments_and_stack(frame_allocator, &mut address_space, program, &header) {
        Ok(()) => Ok((address_space, header.entry_point)),
        Err(error) => {
            address_space.destroy(frame_allocator);
            Err(error)
        }
    }
}

/// The top of the one-page stack that `build_elf_user_address_space` maps.
#[must_use]
pub const fn elf_user_stack_top() -> usize {
    ELF_USER_STACK_VIRTUAL_ADDRESS + PAGE_BYTE_COUNT
}

/// Builds an address space for a tiny raw-code program, kept for the self-tests. Not an ELF loader.
///
/// # Errors
///
/// Fails when memory runs out.
pub fn build_user_address_space(
    frame_allocator: &mut FrameAllocator,
    private_initial_value: u64,
    program: &[u8],
) -> Result<AddressSpace, UserMapError> {
    let mut address_space =
        private_page::build_address_space_with_private_page(frame_allocator, private_initial_value)
            .ok_or(UserMapError::OutOfFrames)?;

    let mapped = user_memory::map_user_region(
        frame_allocator,
        &mut address_space,
        TEST_CODE_ADDRESS,
        PAGE_BYTE_COUNT,
        UserAccess::ReadOnly,
        true,
        program,
    )
    .and_then(|()| {
        user_memory::map_user_region(
            frame_allocator,
            &mut address_space,
            TEST_DATA_ADDRESS,
            PAGE_BYTE_COUNT,
            UserAccess::ReadWrite,
            false,
            &[],
        )
    });

    match mapped {
        Ok(()) => Ok(address_space),
        Err(error) => {
            address_space.destroy(frame_allocator);
            Err(error)
        }
    }
}

/// The top of the data page that `build_user_address_space` maps, used as the stack.
#[must_use]
pub const fn user_stack_top() -> usize {
    TEST_DATA_ADDRESS + PAGE_BYTE_COUNT
}
