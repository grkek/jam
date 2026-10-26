//! Hands out 4 KiB frames of physical memory.

use core::sync::atomic::{AtomicBool, Ordering};

use foundation::{
    device_tree::DeviceTreeSummary,
    frame_bitmap::{FrameBitmap, FrameFreeError},
};

use super::address_translation::{physical_to_virtual, virtual_to_physical};

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
}

fn align_up(address: usize, alignment: usize) -> usize {
    (address + alignment - 1) & !(alignment - 1)
}

/// Builds the frame allocator from the device tree. Succeeds only once.
///
/// The bitmap lives in RAM right after the kernel image. Everything from the start of RAM
/// through the bitmap counts as in use, which also protects the device tree itself.
pub fn initialise_frame_allocator(summary: &DeviceTreeSummary) -> Option<FrameAllocator> {
    if FRAME_ALLOCATOR_INITIALISED.swap(true, Ordering::AcqRel) {
        return None;
    }

    let memory_base_address = usize::try_from(summary.memory_base_address).ok()?;
    let memory_byte_count = usize::try_from(summary.memory_byte_count).ok()?;
    let frame_count = memory_byte_count / FRAME_BYTE_COUNT;
    let storage_word_count = FrameBitmap::required_storage_word_count(frame_count);
    let storage_byte_count = storage_word_count * size_of::<u64>();

    // SAFETY: `__kernel_end` is a symbol the linker script defines. Only its address is taken.
    let kernel_end_address = virtual_to_physical(&raw const __kernel_end as usize);
    let storage_address = align_up(kernel_end_address, FRAME_BYTE_COUNT);
    let reserved_end_address = align_up(storage_address + storage_byte_count, FRAME_BYTE_COUNT);
    if reserved_end_address > memory_base_address + memory_byte_count {
        return None;
    }

    // SAFETY: The range from `storage_address` for `storage_byte_count` bytes lies in RAM above the
    // kernel image, which nothing else uses yet, and RAM is mapped writable at its direct-map
    // address. It is aligned to a frame, so aligned for `u64`. The flag above guarantees this runs
    // once, so no other reference to the range exists. The memory is zeroed before it is viewed as
    // initialised words.
    let storage = unsafe {
        let pointer = physical_to_virtual(storage_address) as *mut u64;
        core::ptr::write_bytes(pointer, 0, storage_word_count);
        core::slice::from_raw_parts_mut(pointer, storage_word_count)
    };

    let mut bitmap = FrameBitmap::new(storage, frame_count)?;
    let reserved_frame_count = (reserved_end_address - memory_base_address) / FRAME_BYTE_COUNT;
    bitmap.mark_used_range(0, reserved_frame_count);

    Some(FrameAllocator {
        bitmap,
        memory_base_address,
    })
}

impl FrameAllocator {
    /// Takes a free frame and returns its physical address.
    pub fn allocate_frame(&mut self) -> Option<usize> {
        let frame_index = self.bitmap.allocate()?;
        Some(self.memory_base_address + frame_index * FRAME_BYTE_COUNT)
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
        self.bitmap.free(offset / FRAME_BYTE_COUNT)
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
