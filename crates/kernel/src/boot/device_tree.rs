//! Finds and reads the device tree handed over at boot.

use foundation::device_tree::{DeviceTreeError, DeviceTreeSummary, parse_device_tree};

use crate::memory::device_tree_blob::device_tree_blob_at;

/// Start of RAM on QEMU `virt`, where the device tree sits when no address is handed over.
const QEMU_VIRT_RAM_BASE_ADDRESS: usize = 0x4000_0000;

/// A device tree that was found and read.
#[derive(Clone, Copy, Debug)]
pub struct LoadedDeviceTree {
    /// Where the blob was actually found, which may differ from the address handed over.
    pub blob_address: usize,
    /// The facts the kernel needs from it.
    pub summary: DeviceTreeSummary,
}

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
///
/// # Errors
///
/// Fails when no device tree is found or the one found is invalid.
pub fn load_device_tree(handed_over_address: usize) -> Result<LoadedDeviceTree, DeviceTreeLoadError> {
    let blob = device_tree_blob_at(handed_over_address)
        .or_else(|| device_tree_blob_at(QEMU_VIRT_RAM_BASE_ADDRESS))
        .ok_or(DeviceTreeLoadError::NotFound)?;
    let summary = parse_device_tree(blob).map_err(DeviceTreeLoadError::Invalid)?;
    Ok(LoadedDeviceTree {
        blob_address: blob.as_ptr() as usize,
        summary,
    })
}
