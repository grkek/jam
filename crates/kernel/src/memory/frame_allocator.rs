//! Hands out 4 KiB frames of physical memory.

use core::sync::atomic::{AtomicBool, Ordering};

use foundation::{
    device_tree::DeviceTreeSummary,
    frame_bitmap::{FrameBitmap, FrameFreeError},
    hardware_description::{HardwareDescription, PhysicalRegion},
    physical_memory::{PhysicalMemoryError, PhysicalMemoryPlan},
};

use super::address_translation::{physical_to_virtual, virtual_to_physical};
use crate::synchronization::spin_lock::SpinLock;

/// Size of one frame of physical memory.
pub const FRAME_BYTE_COUNT: usize = 4096;

static FRAME_ALLOCATOR_INITIALISED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    /// First address after the kernel image, defined by the linker script.
    unsafe static __kernel_end: u8;
}

/// Tracks every frame of RAM. Everything the kernel already occupies is marked in use.
pub struct FrameAllocator {
    bitmap: FrameBitmap<'static>,
    memory_base_address: usize,
    /// Physical reservations that must never be allocated or released.
    physical_memory_plan: PhysicalMemoryPlan,
}

/// Why the allocator could not establish safe ownership of physical RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameAllocatorInitialisationError {
    /// Initialization was already attempted on this machine.
    AlreadyInitialised,
    /// The boot memory extent cannot be represented by JAM's address space.
    UnsupportedAddress,
    /// Firmware passed an empty or invalid device-tree memory extent.
    InvalidDeviceTree,
    /// Discovery or firmware reservations are incompatible with the current bootstrap.
    UnsafeMemoryLayout(PhysicalMemoryError),
    /// A usable bitmap could not be constructed.
    BitmapUnavailable,
}

/// Constructs a frame allocator only after confirming ownership of every allocatable page.
///
/// The first discovered RAM bank is the only one the bootstrap currently maps. Other banks
/// are checked for overlap but not used until the kernel establishes their direct mappings.
/// The bitmap is initialized in unreserved RAM beyond the kernel image, and all firmware,
/// device-tree, kernel-image, and bitmap frames are marked unavailable.
///
/// # Errors
///
/// Fails closed if discovery is inconsistent, the device tree requests an unresolved dynamic
/// reservation, `no-map` memory intersects the bootstrap direct map, or bitmap space runs out.
pub fn initialise_frame_allocator(
    summary: &DeviceTreeSummary,
    hardware: &HardwareDescription,
    device_tree_address: usize,
    device_tree_byte_count: usize,
) -> Result<FrameAllocator, FrameAllocatorInitialisationError> {
    if FRAME_ALLOCATOR_INITIALISED.swap(true, Ordering::AcqRel) {
        return Err(FrameAllocatorInitialisationError::AlreadyInitialised);
    }

    let memory_base_address = usize::try_from(summary.memory_base_address)
        .map_err(|_| FrameAllocatorInitialisationError::UnsupportedAddress)?;
    let memory_byte_count = usize::try_from(summary.memory_byte_count)
        .map_err(|_| FrameAllocatorInitialisationError::UnsupportedAddress)?;
    let frame_count = memory_byte_count / FRAME_BYTE_COUNT;
    if frame_count == 0 {
        return Err(FrameAllocatorInitialisationError::UnsupportedAddress);
    }
    let storage_word_count = FrameBitmap::required_storage_word_count(frame_count);
    let storage_byte_count = storage_word_count
        .checked_mul(size_of::<u64>())
        .ok_or(FrameAllocatorInitialisationError::UnsupportedAddress)?;

    // SAFETY: Only the address of the linker-defined kernel boundary is read.
    let kernel_end_address = virtual_to_physical(&raw const __kernel_end as usize);
    let managed_bank = PhysicalRegion::new(summary.memory_base_address, summary.memory_byte_count)
        .ok_or(FrameAllocatorInitialisationError::UnsupportedAddress)?;
    let device_tree_region = PhysicalRegion::new(
        u64::try_from(device_tree_address)
            .map_err(|_| FrameAllocatorInitialisationError::UnsupportedAddress)?,
        u64::try_from(device_tree_byte_count)
            .map_err(|_| FrameAllocatorInitialisationError::UnsupportedAddress)?,
    )
    .ok_or(FrameAllocatorInitialisationError::InvalidDeviceTree)?;
    let plan = PhysicalMemoryPlan::new(
        managed_bank,
        crate::platform::current::BOOTSTRAP_MAPPED_RAM,
        hardware.memory_regions(),
        hardware.reserved_regions(),
        hardware.dynamic_reservation_count(),
        u64::try_from(kernel_end_address)
            .map_err(|_| FrameAllocatorInitialisationError::UnsupportedAddress)?,
        device_tree_region,
        u64::try_from(storage_byte_count)
            .map_err(|_| FrameAllocatorInitialisationError::UnsupportedAddress)?,
    )
    .map_err(FrameAllocatorInitialisationError::UnsafeMemoryLayout)?;

    let bitmap_physical_address = usize::try_from(plan.bitmap_storage().start_address)
        .map_err(|_| FrameAllocatorInitialisationError::UnsupportedAddress)?;
    // SAFETY: `plan` has proved the selected pages are within the boot-mapped RAM bank and do
    // not overlap the kernel image, firmware reservations, or the device tree. They are not
    // shared by another allocator. The address is frame aligned, hence aligned for `u64`.
    // We initialize exactly the word-count owned by this bitmap before creating the slice.
    let storage = unsafe {
        let pointer = physical_to_virtual(bitmap_physical_address) as *mut u64;
        core::ptr::write_bytes(pointer, 0, storage_word_count);
        core::slice::from_raw_parts_mut(pointer, storage_word_count)
    };
    let mut bitmap = FrameBitmap::new(storage, frame_count)
        .ok_or(FrameAllocatorInitialisationError::BitmapUnavailable)?;

    // Round every reservation outward to frame boundaries. Firmware may reserve unaligned bytes.
    for &region in plan.protected_regions() {
        if let Some((first_index, count)) = plan.protected_frame_interval(region) {
            bitmap.mark_used_range(first_index, count);
        }
    }

    Ok(FrameAllocator {
        bitmap,
        memory_base_address,
        physical_memory_plan: plan,
    })
}

impl FrameAllocator {
    /// Takes a free frame and returns its physical address.
    pub fn allocate_frame(&mut self) -> Option<usize> {
        let frame_index = self.bitmap.allocate()?;
        let frame_address = self
            .memory_base_address
            .checked_add(frame_index.checked_mul(FRAME_BYTE_COUNT)?)?;
        assert!(
            !self
                .physical_memory_plan
                .is_protected_frame(frame_address as u64),
            "BUG: allocator allocated protected frame {frame_address:#x}"
        );
        Some(frame_address)
    }

    /// Returns a frame to the allocator.
    ///
    /// # Errors
    ///
    /// Fails when the address is not a frame this allocator tracks, or the frame is already free.
    pub fn free_frame(&mut self, frame_address: usize) -> Result<(), FrameFreeError> {
        let offset = frame_address
            .checked_sub(self.memory_base_address)
            .ok_or(FrameFreeError::OutOfRange)?;

        if offset % FRAME_BYTE_COUNT != 0 {
            return Err(FrameFreeError::OutOfRange);
        }

        let frame_index = offset / FRAME_BYTE_COUNT;

        // A protected frame remains owned by firmware or the bootstrap even when its
        // bitmap bit is set; it must never become available through `free_frame`.
        if self
            .physical_memory_plan
            .is_protected_frame(frame_address as u64)
        {
            return Err(FrameFreeError::NotAllocated);
        }
        self.bitmap.free(frame_index)
    }

    /// Frames of RAM in total.
    #[must_use]
    pub const fn total_frame_count(&self) -> usize {
        self.bitmap.frame_count()
    }

    /// Frames currently free.
    #[must_use]
    pub const fn free_frame_count(&self) -> usize {
        self.bitmap.free_frame_count()
    }
}

static GLOBAL_FRAME_ALLOCATOR: SpinLock<Option<FrameAllocator>> = SpinLock::new(None);

/// Hands the allocator to the rest of the kernel. Called once, after boot setup.
pub fn install_frame_allocator(frame_allocator: FrameAllocator) {
    *GLOBAL_FRAME_ALLOCATOR.lock() = Some(frame_allocator);
}

/// Runs `operation` with the allocator, or returns `None` before it is installed.
pub fn with_frame_allocator<Output>(
    operation: impl FnOnce(&mut FrameAllocator) -> Output,
) -> Option<Output> {
    GLOBAL_FRAME_ALLOCATOR.lock().as_mut().map(operation)
}
