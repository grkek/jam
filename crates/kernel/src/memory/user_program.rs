//! Loads static AArch64 ELF64 executables into a JAM actor address space.
//!
//! Requires a 39-bit TTBR0 user virtual-address range and 4 KiB pages.
//! This loader does not support PIE, dynamic linking, or ELF relocations.

use core::{cmp, mem, ptr};

use foundation::page_table_entry::{PAGE_BYTE_COUNT, UserAccess};

use crate::memory::user_memory::{TEST_CODE_ADDRESS, TEST_DATA_ADDRESS};

use super::{
    address_space::AddressSpace,
    frame_allocator::FrameAllocator,
    private_page,
    user_memory::{self, UserMapError},
};

const ELF_MAGIC_BYTES: &[u8] = &[0x7f, b'E', b'L', b'F'];

const USER_VIRTUAL_ADDRESS_LIMIT: u64 = 1 << 39;
const ELF_USER_STACK_VIRTUAL_ADDRESS: usize = 0x0000_007f_ffff_0000;

const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LITTLE_ENDIAN: u8 = 1;
const ELF_CURRENT_VERSION: u8 = 1;
const ELF_TYPE_EXECUTABLE: u16 = 2;
const ELF_MACHINE_AARCH64: u16 = 183;
const ELF_VERSION_CURRENT: u32 = 1;

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

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Header {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_hsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64ProgramHeader {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

/// Converts a malformed ELF or unsupported mapping request into InvalidRegion.
fn invalid_region() -> UserMapError {
    UserMapError::InvalidRegion
}

fn page_floor(address: u64) -> u64 {
    address & !((PAGE_BYTE_COUNT as u64) - 1)
}

fn page_ceiling(address: u64) -> Result<u64, UserMapError> {
    address
        .checked_add((PAGE_BYTE_COUNT as u64) - 1)
        .map(page_floor)
        .ok_or_else(invalid_region)
}

fn intervals_overlap(start_a: u64, end_a: u64, start_b: u64, end_b: u64) -> bool {
    start_a < end_b && start_b < end_a
}

fn read_program_header(
    program: &[u8],
    header: &Elf64Header,
    index: usize,
) -> Result<Elf64ProgramHeader, UserMapError> {
    let header_table_offset = usize::try_from(header.e_phoff).map_err(|_| invalid_region())?;
    let entry_offset = index
        .checked_mul(mem::size_of::<Elf64ProgramHeader>())
        .and_then(|offset| header_table_offset.checked_add(offset))
        .ok_or_else(invalid_region)?;
    let entry_end = entry_offset
        .checked_add(mem::size_of::<Elf64ProgramHeader>())
        .ok_or_else(invalid_region)?;
    let entry_bytes = program
        .get(entry_offset..entry_end)
        .ok_or_else(invalid_region)?;

    // The ELF format is little-endian and is decoded on a little-endian AArch64 target.
    Ok(unsafe { ptr::read_unaligned(entry_bytes.as_ptr().cast::<Elf64ProgramHeader>()) })
}

/// Validates the entire program *before* allocating user frames.
/// Returns the ELF entry address.
fn validate_elf(program: &[u8]) -> Result<(Elf64Header, u64), UserMapError> {
    if program.len() < mem::size_of::<Elf64Header>() {
        return Err(invalid_region());
    }
    let header = unsafe { ptr::read_unaligned(program.as_ptr().cast::<Elf64Header>()) };

    if !header.e_ident.starts_with(ELF_MAGIC_BYTES)
        || header.e_ident[4] != ELF_CLASS_64
        || header.e_ident[5] != ELF_DATA_LITTLE_ENDIAN
        || header.e_ident[6] != ELF_CURRENT_VERSION
        || header.e_type != ELF_TYPE_EXECUTABLE
        || header.e_machine != ELF_MACHINE_AARCH64
        || header.e_version != ELF_VERSION_CURRENT
        || header.e_hsize as usize != mem::size_of::<Elf64Header>()
        || header.e_phentsize as usize != mem::size_of::<Elf64ProgramHeader>()
        || header.e_phnum == 0
    {
        return Err(invalid_region());
    }

    let header_table_offset = usize::try_from(header.e_phoff).map_err(|_| invalid_region())?;
    let header_table_byte_count = (header.e_phnum as usize)
        .checked_mul(mem::size_of::<Elf64ProgramHeader>())
        .ok_or_else(invalid_region)?;
    let header_table_end = header_table_offset
        .checked_add(header_table_byte_count)
        .ok_or_else(invalid_region)?;
    if header_table_end > program.len() {
        return Err(invalid_region());
    }

    let entry_point = header.e_entry;
    if entry_point < PAGE_BYTE_COUNT as u64
        || entry_point % 4 != 0
        || entry_point >= USER_VIRTUAL_ADDRESS_LIMIT
    {
        return Err(invalid_region());
    }

    let stack_start = ELF_USER_STACK_VIRTUAL_ADDRESS as u64;
    let stack_end = stack_start + PAGE_BYTE_COUNT as u64;
    let mut found_loadable_segment = false;
    let mut entry_is_file_backed_executable = false;

    for index in 0..header.e_phnum as usize {
        let segment = read_program_header(program, &header, index)?;
        if segment.p_type == PROGRAM_HEADER_TYPE_INTERPRETER
            || segment.p_type == PROGRAM_HEADER_TYPE_DYNAMIC
        {
            // JAM does not provide a dynamic linker/relocation runtime yet.
            return Err(invalid_region());
        }
        if segment.p_type != PROGRAM_HEADER_TYPE_LOAD {
            continue;
        }
        if segment.p_filesz > segment.p_memsz {
            return Err(invalid_region());
        }

        let segment_file_offset =
            usize::try_from(segment.p_offset).map_err(|_| invalid_region())?;
        let segment_file_size = usize::try_from(segment.p_filesz).map_err(|_| invalid_region())?;
        let segment_file_end = segment_file_offset
            .checked_add(segment_file_size)
            .ok_or_else(invalid_region)?;
        if segment_file_end > program.len() {
            return Err(invalid_region());
        }

        if segment.p_align > 1
            && (!segment.p_align.is_power_of_two()
                || segment.p_vaddr % segment.p_align != segment.p_offset % segment.p_align)
        {
            return Err(invalid_region());
        }
        // The file and VA offsets must also be congruent modulo the page granule.
        if segment.p_vaddr % PAGE_BYTE_COUNT as u64 != segment.p_offset % PAGE_BYTE_COUNT as u64 {
            return Err(invalid_region());
        }

        let writable = segment.p_flags & PROGRAM_FLAG_WRITE != 0;
        let executable = segment.p_flags & PROGRAM_FLAG_EXECUTE != 0;
        if writable && executable {
            return Err(invalid_region()); // W^X
        }

        if segment.p_memsz == 0 {
            continue;
        }
        found_loadable_segment = true;
        let segment_end = segment
            .p_vaddr
            .checked_add(segment.p_memsz)
            .ok_or_else(invalid_region)?;
        if segment.p_vaddr < PAGE_BYTE_COUNT as u64
            || segment_end > USER_VIRTUAL_ADDRESS_LIMIT
            || intervals_overlap(segment.p_vaddr, segment_end, stack_start, stack_end)
        {
            return Err(invalid_region());
        }

        // A minimal static loader rejects byte-range overlaps between PT_LOAD segments.
        // It *does* support distinct segments sharing the same 4 KiB page.
        for prior_index in 0..index {
            let prior = read_program_header(program, &header, prior_index)?;
            if prior.p_type != PROGRAM_HEADER_TYPE_LOAD || prior.p_memsz == 0 {
                continue;
            }
            let prior_end = prior
                .p_vaddr
                .checked_add(prior.p_memsz)
                .ok_or_else(invalid_region)?;
            if intervals_overlap(segment.p_vaddr, segment_end, prior.p_vaddr, prior_end) {
                return Err(invalid_region());
            }
        }

        let file_virtual_end = segment
            .p_vaddr
            .checked_add(segment.p_filesz)
            .ok_or_else(invalid_region)?;
        if executable
            && entry_point >= segment.p_vaddr
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

    Ok((header, entry_point))
}

/// Returns whether an earlier PT_LOAD segment already enumerated this physical page.
fn page_was_visited(
    program: &[u8],
    elf_header: &Elf64Header,
    current_segment_index: usize,
    page_address: u64,
) -> Result<bool, UserMapError> {
    for prior_index in 0..current_segment_index {
        let prior = read_program_header(program, elf_header, prior_index)?;
        if prior.p_type != PROGRAM_HEADER_TYPE_LOAD || prior.p_memsz == 0 {
            continue;
        }
        let prior_end = prior
            .p_vaddr
            .checked_add(prior.p_memsz)
            .ok_or_else(invalid_region)?;
        if page_floor(prior.p_vaddr) <= page_address && page_address < page_ceiling(prior_end)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Compose one complete page from all loadable segments that touch it.
/// Supports unaligned virtual addresses and distinct segments sharing a page.
fn build_segment_page(
    program: &[u8],
    elf_header: &Elf64Header,
    page_address: u64,
    page_bytes: &mut [u8; PAGE_BYTE_COUNT],
) -> Result<(UserAccess, bool), UserMapError> {
    let page_end = page_address + PAGE_BYTE_COUNT as u64;
    let mut writable = false;
    let mut executable = false;
    page_bytes.fill(0);

    for index in 0..elf_header.e_phnum as usize {
        let segment = read_program_header(program, elf_header, index)?;
        if segment.p_type != PROGRAM_HEADER_TYPE_LOAD || segment.p_memsz == 0 {
            continue;
        }
        let segment_end = segment.p_vaddr + segment.p_memsz; // checked in validate_elf
        if !intervals_overlap(page_address, page_end, segment.p_vaddr, segment_end) {
            continue;
        }

        writable |= segment.p_flags & PROGRAM_FLAG_WRITE != 0;
        executable |= segment.p_flags & PROGRAM_FLAG_EXECUTE != 0;
        if writable && executable {
            // A shared page cannot simultaneously be RW and executable under W^X.
            return Err(invalid_region());
        }

        let file_end = segment.p_vaddr + segment.p_filesz; // checked in validate_elf
        let copy_start = cmp::max(page_address, segment.p_vaddr);
        let copy_end = cmp::min(page_end, file_end);
        if copy_start >= copy_end {
            continue; // zero-initialized .bss or unused part of a page
        }

        let destination_offset = (copy_start - page_address) as usize;
        let copy_length = (copy_end - copy_start) as usize;
        let source_offset = (segment.p_offset + (copy_start - segment.p_vaddr)) as usize;
        page_bytes[destination_offset..destination_offset + copy_length]
            .copy_from_slice(&program[source_offset..source_offset + copy_length]);
    }

    let access = if writable {
        UserAccess::ReadWrite
    } else {
        UserAccess::ReadOnly
    };
    Ok((access, executable))
}

/// Validates and maps a statically linked AArch64 ELF64 executable and a one-page user stack.
///
/// `map_user_region` is expected to copy the supplied data synchronously and zero-fill
/// all other bytes. Because executable segments are composed page-by-page, this function
/// uses a 4 KiB temporary buffer on the kernel stack.
///
/// NOTE: the memory-mapping layer must perform appropriate instruction-cache maintenance
/// after copying executable bytes through the kernel's writable alias.
pub fn build_elf_user_address_space(
    frame_allocator: &mut FrameAllocator,
    private_initial_value: u64,
    program: &[u8],
) -> Result<(AddressSpace, u64), UserMapError> {
    let (elf_header, entry_point) = validate_elf(program)?;

    // This loader allocates every loadable page eagerly. Fail before partially mapping
    // a clearly oversized image. This count conservatively includes shared pages twice.
    let mut requested_page_count = 2u64; // private page and stack; page tables need more
    for index in 0..elf_header.e_phnum as usize {
        let segment = read_program_header(program, &elf_header, index)?;
        if segment.p_type != PROGRAM_HEADER_TYPE_LOAD || segment.p_memsz == 0 {
            continue;
        }
        let start = page_floor(segment.p_vaddr);
        let end = page_ceiling(segment.p_vaddr + segment.p_memsz)?;
        requested_page_count = requested_page_count
            .checked_add((end - start) / PAGE_BYTE_COUNT as u64)
            .ok_or_else(invalid_region)?;
    }
    if requested_page_count >= frame_allocator.free_frame_count() as u64 {
        return Err(UserMapError::OutOfFrames);
    }

    let mut address_space =
        private_page::build_address_space_with_private_page(frame_allocator, private_initial_value)
            .ok_or(UserMapError::OutOfFrames)?;

    // Enumerate each page exactly once, even if two PT_LOAD segments share it.
    for index in 0..elf_header.e_phnum as usize {
        let segment = read_program_header(program, &elf_header, index)?;
        if segment.p_type != PROGRAM_HEADER_TYPE_LOAD || segment.p_memsz == 0 {
            continue;
        }

        let segment_end = segment.p_vaddr + segment.p_memsz; // checked in validate_elf
        let end_page = page_ceiling(segment_end)?;
        let mut page_address = page_floor(segment.p_vaddr);
        while page_address < end_page {
            if !page_was_visited(program, &elf_header, index, page_address)? {
                let mut page_bytes = [0u8; PAGE_BYTE_COUNT];
                let (access, executable) =
                    build_segment_page(program, &elf_header, page_address, &mut page_bytes)?;
                user_memory::map_user_region(
                    frame_allocator,
                    &mut address_space,
                    page_address as usize,
                    PAGE_BYTE_COUNT,
                    access,
                    executable,
                    &page_bytes,
                )?;
            }
            page_address += PAGE_BYTE_COUNT as u64;
        }
    }

    user_memory::map_user_region(
        frame_allocator,
        &mut address_space,
        ELF_USER_STACK_VIRTUAL_ADDRESS,
        PAGE_BYTE_COUNT,
        UserAccess::ReadWrite,
        false,
        &[],
    )?;

    Ok((address_space, entry_point))
}

#[must_use]
/// :ditto:
pub const fn elf_user_stack_top() -> usize {
    ELF_USER_STACK_VIRTUAL_ADDRESS + PAGE_BYTE_COUNT
}

/// Existing tiny raw-code actor helper retained for self-tests (not an ELF loader).
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
