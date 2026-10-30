//! Finds and reads the device tree handed over at boot.

use foundation::device_tree::{DeviceTreeError, DeviceTreeSummary, parse_device_tree};
use foundation::hardware_description::{DiscoveryError, HardwareDescription, discover_hardware};

use crate::memory::{
    address_translation::virtual_to_physical, device_tree_blob::device_tree_blob_at,
};

/// Start of RAM on QEMU `virt`, where the device tree sits when no address is handed over.
const QEMU_VIRT_RAM_BASE_ADDRESS: usize = 0x4000_0000;

/// A device tree that was found and read.
#[derive(Clone, Copy, Debug)]
pub struct LoadedDeviceTree {
    /// The physical address where the blob was actually found, which may differ from the
    /// address handed over.
    pub blob_address: usize,
    /// Exact number of firmware device-tree bytes to keep out of the frame allocator.
    pub blob_byte_count: usize,
    /// The facts the kernel needs from it.
    pub summary: DeviceTreeSummary,
    /// Additional memory reservations and CPU affinities; informational until Phase 3.
    pub hardware_description: Result<HardwareDescription, DiscoveryError>,
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
/// Tries the physical address the firmware handed over first, then the start of RAM.
///
/// # Errors
///
/// Fails when no device tree is found or the one found is invalid.
pub fn load_device_tree(
    handed_over_address: usize,
) -> Result<LoadedDeviceTree, DeviceTreeLoadError> {
    let blob = device_tree_blob_at(handed_over_address)
        .or_else(|| device_tree_blob_at(QEMU_VIRT_RAM_BASE_ADDRESS))
        .ok_or(DeviceTreeLoadError::NotFound)?;
    let summary = parse_device_tree(blob).map_err(DeviceTreeLoadError::Invalid)?;
    // A discovery error is reported by the boot core but cannot alter Phase 1 boot behavior.
    let hardware_description = discover_hardware(blob);
    Ok(LoadedDeviceTree {
        blob_address: virtual_to_physical(blob.as_ptr() as usize),
        blob_byte_count: blob.len(),
        summary,
        hardware_description,
    })
}
