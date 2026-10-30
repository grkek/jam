//! Accesses a firmware-provided device tree through the temporary RAM direct map.

use core::ptr::read_volatile;

use foundation::device_tree::{DEVICE_TREE_HEADER_BYTE_COUNT, DEVICE_TREE_MAGIC};

use super::address_translation::physical_to_virtual;

const TOTAL_SIZE_OFFSET: usize = 4;
const WORD_ALIGNMENT: usize = 4;
/// Upper limit for a firmware-provided flattened device tree.
const DEVICE_TREE_BYTE_MAXIMUM: usize = 1024 * 1024;

/// Reads a device-tree blob only when its maximum extent is safely boot-mapped.
///
/// `address` must be the physical address supplied by trusted boot firmware or
/// the board's explicitly permitted fallback. An invalid pointer fails closed.
#[must_use]
pub fn device_tree_blob_at(address: usize) -> Option<&'static [u8]> {
    let mapped_ram = crate::platform::current::BOOTSTRAP_MAPPED_RAM;
    let mapped_start = usize::try_from(mapped_ram.start_address).ok()?;
    let mapped_length = usize::try_from(mapped_ram.byte_count).ok()?;
    let mapped_end = mapped_start.checked_add(mapped_length)?;
    let maximum_end = address.checked_add(DEVICE_TREE_BYTE_MAXIMUM)?;

    if address == 0
        || address % WORD_ALIGNMENT != 0
        || address < mapped_start
        || maximum_end > mapped_end
    {
        return None;
    }

    let virtual_address = physical_to_virtual(address);

    // SAFETY: The pointer is firmware-provided or a board-approved fallback.
    // The whole maximum-length interval is within boot-mapped RAM. The 32-bit
    // header registers are aligned, and the blob remains firmware-owned and
    // unchanged throughout parsing. The resulting slice cannot exceed 1 MiB.
    unsafe {
        let magic = u32::from_be(read_volatile(virtual_address as *const u32));
        if magic != DEVICE_TREE_MAGIC {
            return None;
        }

        let total_size = usize::try_from(u32::from_be(read_volatile(
            (virtual_address + TOTAL_SIZE_OFFSET) as *const u32,
        )))
        .ok()?;
        if !(DEVICE_TREE_HEADER_BYTE_COUNT..=DEVICE_TREE_BYTE_MAXIMUM).contains(&total_size) {
            return None;
        }

        Some(core::slice::from_raw_parts(
            virtual_address as *const u8,
            total_size,
        ))
    }
}
