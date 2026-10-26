//! Raw access to translation tables stored in physical frames.

use core::ptr::{read_volatile, write_bytes, write_volatile};

use foundation::page_table_entry::{ENTRIES_PER_TABLE, PageTableEntry};

use super::frame_allocator::{FRAME_BYTE_COUNT, FrameAllocator};

/// One frame holding a translation table. Every table the kernel builds is a frame it allocated
/// itself, which is why reading and writing entries is sound.
#[derive(Clone, Copy)]
pub struct TableFrame {
    physical_address: usize,
}

impl TableFrame {
    /// Allocates a frame and fills it with invalid entries.
    pub fn allocate(frame_allocator: &mut FrameAllocator) -> Option<Self> {
        let physical_address = frame_allocator.allocate_frame()?;
        // SAFETY: The frame was just allocated, so the kernel owns all `FRAME_BYTE_COUNT` bytes
        // and nothing else references them. Physical and virtual addresses are identical because
        // the MMU is either off or running the identity map.
        unsafe {
            write_bytes(physical_address as *mut u8, 0, FRAME_BYTE_COUNT);
        }
        Some(Self { physical_address })
    }

    /// Wraps a table that an earlier entry of ours points to.
    pub(super) const fn from_physical_address(physical_address: usize) -> Self {
        Self { physical_address }
    }

    /// Physical address of the table.
    #[must_use]
    pub const fn physical_address(self) -> usize {
        self.physical_address
    }

    /// Reads the entry at `index`, or `None` when the index is outside the table.
    #[must_use]
    pub fn read(self, index: usize) -> Option<PageTableEntry> {
        if index >= ENTRIES_PER_TABLE {
            return None;
        }
        // SAFETY: A `TableFrame` always refers to a table frame the kernel owns, `index` is inside
        // it, and entries are naturally aligned 64-bit words.
        let raw_value = unsafe { read_volatile((self.physical_address as *const u64).add(index)) };
        Some(PageTableEntry::from_raw_value(raw_value))
    }

    /// Writes the entry at `index`. Returns `None` when the index is outside the table.
    #[must_use]
    pub fn write(self, index: usize, entry: PageTableEntry) -> Option<()> {
        if index >= ENTRIES_PER_TABLE {
            return None;
        }
        // SAFETY: Same as `read`. The kernel is the only writer of its own tables.
        unsafe {
            write_volatile(
                (self.physical_address as *mut u64).add(index),
                entry.raw_value(),
            );
        }
        Some(())
    }
}
