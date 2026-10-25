//! Finds and reads the device tree handed over at boot.

use foundation::device_tree::{DeviceTreeError, DeviceTreeSummary, parse_device_tree};

use crate::memory::device_tree_blob::device_tree_blob_at;

/// Start of RAM on QEMU `virt`, where the device tree sits when no address is handed over.
const QEMU_VIRT_RAM_BASE_ADDRESS: usize = 0x4000_0000;

/// Why the device tree could not be used.
#[derive(Debug)]
pub enum DeviceTreeLoadError {
    /// No valid device tree header was found.
    NotFound,
    /// A device tree was found but could not be read.
    Invalid(DeviceTreeError),
}

/// Locates the device tree and reads the facts the kernel needs from it.
///
/// Tries the address the firmware handed over first, then the start of RAM.
pub fn load_device_tree_summary(
    handed_over_address: usize,
) -> Result<DeviceTreeSummary, DeviceTreeLoadError> {
    // If x0 is 0 (ELF bare-metal boot), skip trying x0 and go straight to the base of RAM.
    let blob = if handed_over_address != 0 {
        device_tree_blob_at(handed_over_address)
    } else {
        None
    }
    .or_else(|| device_tree_blob_at(QEMU_VIRT_RAM_BASE_ADDRESS))
    .ok_or(DeviceTreeLoadError::NotFound)?;

    parse_device_tree(blob).map_err(DeviceTreeLoadError::Invalid)
}