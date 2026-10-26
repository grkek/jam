//! Access to the device tree the firmware leaves in memory.

use core::ptr::read_volatile;

use foundation::device_tree::{DEVICE_TREE_HEADER_BYTE_COUNT, DEVICE_TREE_MAGIC};

use super::address_translation::physical_to_virtual;

const TOTAL_SIZE_OFFSET: usize = 4;
const WORD_ALIGNMENT: usize = 4;
/// Refuse blobs larger than this, so a corrupt header cannot make us read all of memory.
const DEVICE_TREE_BYTE_MAXIMUM: usize = 1024 * 1024;

/// Returns the device tree blob at the physical `address`, or `None` when no valid header is there.
#[must_use]
pub fn device_tree_blob_at(address: usize) -> Option<&'static [u8]> {
    if address == 0 || address % WORD_ALIGNMENT != 0 {
        return None;
    }
    let virtual_address = physical_to_virtual(address);

    // SAFETY: `address` is either what the firmware handed over or the start of RAM, both inside
    // RAM, which the boot tables map readable at its direct-map address. The header words are
    // aligned (checked above) and only read. The firmware leaves the blob untouched, and the
    // returned slice is bounded by `DEVICE_TREE_BYTE_MAXIMUM`.
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
