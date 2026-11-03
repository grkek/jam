//! Checked copies between kernel buffers and an actor's ordinary EL0 pages.
//!
//! Callers hold the actor control-block lock throughout validation and copying.
//! Pages are reached through the kernel direct map, never an unchecked EL0 pointer.
//! The entire range is validated before copying, including page crossings.

use super::{address_space::AddressSpace, address_translation::physical_to_virtual};
use foundation::{
    page_table_entry::{NORMAL_MEMORY_ATTRIBUTE_INDEX, PAGE_BYTE_COUNT},
    system_call_status::SystemCallStatus,
};

/// Resolves one ordinary user page with the requested access.
fn resolve(
    space: &AddressSpace,
    address: usize,
    writable: bool,
) -> Result<usize, SystemCallStatus> {
    let entry = space
        .lookup_page(address)
        .ok_or(SystemCallStatus::InvalidUserMemory)?;
    if !entry.is_table_or_page()
        || !entry.is_user_accessible()
        || (writable && entry.is_read_only())
        || (entry.raw_value() >> 2) & 7 != NORMAL_MEMORY_ATTRIBUTE_INDEX
    {
        return Err(SystemCallStatus::InvalidUserMemory);
    }
    let physical =
        usize::try_from(entry.output_address()).map_err(|_| SystemCallStatus::InvalidUserMemory)?;
    Ok(physical_to_virtual(physical) + address % PAGE_BYTE_COUNT)
}

/// Validates a lower-half range and every page permission before any copying.
fn validate(
    space: &AddressSpace,
    address: usize,
    length: usize,
    writable: bool,
) -> Result<(), SystemCallStatus> {
    if length == 0 {
        return Ok(());
    }
    let end = address
        .checked_add(length)
        .ok_or(SystemCallStatus::InvalidUserMemory)?;
    if address < PAGE_BYTE_COUNT || end > (1usize << 39) {
        return Err(SystemCallStatus::InvalidUserMemory);
    }
    let mut cursor = address;
    while cursor < end {
        resolve(space, cursor, writable)?;
        cursor += (PAGE_BYTE_COUNT - cursor % PAGE_BYTE_COUNT).min(end - cursor);
    }
    Ok(())
}

/// Copies validated user bytes into a distinct kernel-owned buffer.
///
/// The actor lock must protect the supplied address space for this entire call.
/// # Errors
/// Rejects overflow, unmapped memory, kernel-only pages, and device mappings.
pub fn copy_from_actor(
    space: &AddressSpace,
    address: usize,
    output: &mut [u8],
) -> Result<(), SystemCallStatus> {
    validate(space, address, output.len(), false)?;
    let mut copied = 0;
    while copied < output.len() {
        let cursor = address + copied;
        let count = (PAGE_BYTE_COUNT - cursor % PAGE_BYTE_COUNT).min(output.len() - copied);
        let source = resolve(space, cursor, false)?;
        let chunk = output
            .get_mut(copied..copied + count)
            .ok_or(SystemCallStatus::InvalidArgument)?;
        // SAFETY: The whole user range was checked under its owner's lock. The
        // ordinary RAM page remains live; the kernel buffer is distinct storage.
        unsafe {
            core::ptr::copy_nonoverlapping(source as *const u8, chunk.as_mut_ptr(), count);
        }
        copied += count;
    }
    Ok(())
}

/// Copies kernel bytes into validated writable user pages.
///
/// The actor lock must protect the supplied address space for this entire call.
/// # Errors
/// Rejects invalid ranges and nonwritable pages before writing any bytes.
pub fn copy_to_actor(
    space: &AddressSpace,
    address: usize,
    input: &[u8],
) -> Result<(), SystemCallStatus> {
    validate(space, address, input.len(), true)?;
    let mut copied = 0;
    while copied < input.len() {
        let cursor = address + copied;
        let count = (PAGE_BYTE_COUNT - cursor % PAGE_BYTE_COUNT).min(input.len() - copied);
        let destination = resolve(space, cursor, true)?;
        let chunk = input
            .get(copied..copied + count)
            .ok_or(SystemCallStatus::InvalidArgument)?;
        // SAFETY: All destinations are writable ordinary user RAM, held live by
        // the actor lock. The source is a distinct kernel-owned buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(chunk.as_ptr(), destination as *mut u8, count);
        }
        copied += count;
    }
    Ok(())
}
