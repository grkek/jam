//! Finds and reads the flattened device tree provided by the boot firmware.

use foundation::device_tree::{DeviceTreeError, DeviceTreeSummary, parse_device_tree};
use foundation::hardware_description::{DiscoveryError, HardwareDescription, discover_hardware};

use crate::memory::{
    address_translation::virtual_to_physical, device_tree_blob::device_tree_blob_at,
};

/// A device tree that was found and validated.
#[derive(Clone, Copy, Debug)]
pub struct LoadedDeviceTree {
    /// Physical address of the blob that was actually read.
    pub blob_address: usize,
    /// Total bytes occupied by the blob; reserved from the frame allocator.
    pub blob_byte_count: usize,
    /// Boot-critical memory, CPU, and firmware information.
    pub summary: DeviceTreeSummary,
    /// Discovered memory reservations and CPU affinities.
    pub hardware_description: Result<HardwareDescription, DiscoveryError>,
}

/// Why the device tree could not be used.
#[derive(Debug)]
pub enum DeviceTreeLoadError {
    /// No valid device tree header was found at a permitted address.
    NotFound,
    /// A found device tree could not be parsed.
    Invalid(DeviceTreeError),
}

/// Loads the firmware-provided tree, optionally using a board-approved fallback.
///
/// # Errors
///
/// Returns an error when neither physical address contains a valid tree, or
/// when the tree is invalid. Only boot-mapped RAM may be inspected.
pub fn load_device_tree(
    handed_over_address: usize,
) -> Result<LoadedDeviceTree, DeviceTreeLoadError> {
    let blob = device_tree_blob_at(handed_over_address)
        .or_else(|| {
            crate::platform::current::DEVICE_TREE_FALLBACK_ADDRESS.and_then(device_tree_blob_at)
        })
        .ok_or(DeviceTreeLoadError::NotFound)?;
    let summary = parse_device_tree(blob).map_err(DeviceTreeLoadError::Invalid)?;
    let hardware_description = discover_hardware(blob);

    Ok(LoadedDeviceTree {
        blob_address: virtual_to_physical(blob.as_ptr() as usize),
        blob_byte_count: blob.len(),
        summary,
        hardware_description,
    })
}
