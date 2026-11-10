//! A two-level table of addresses, built at boot from physical frames.
//!
//! The top level is a small static array of page pointers; each page is one frame of entries.
//! Only the pages an entry count needs are allocated, so memory follows the count and never
//! a compile-time ceiling.

use core::{
    ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use crate::memory::{address_translation::physical_to_virtual, frame_allocator::FrameAllocator};

const FRAME_BYTE_COUNT: usize = 4096;

/// Entries in one page of the directory.
const ENTRIES_PER_PAGE: usize = FRAME_BYTE_COUNT / size_of::<AtomicUsize>();

/// Most entries any directory can hold.
///
/// This is the architectural limit of a 16-bit address-space identifier, not a memory policy.
pub const MAXIMUM_ENTRY_COUNT: usize = 1 << 16;

const MAXIMUM_PAGE_COUNT: usize = MAXIMUM_ENTRY_COUNT / ENTRIES_PER_PAGE;

/// Why a directory could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryError {
    /// The directory was already built.
    AlreadyInitialised,
    /// No entries were requested.
    Empty,
    /// More entries were requested than a directory can hold.
    ExceedsCapacity,
    /// The physical frame allocator ran out of frames.
    OutOfFrames,
}

/// A table of nonzero addresses, indexed by slot. Zero means the entry is not set.
pub struct PagedDirectory {
    /// Base address of every page. The pages themselves come from the frame allocator.
    pages: [AtomicPtr<AtomicUsize>; MAXIMUM_PAGE_COUNT],
    /// Number of entries that exist. Zero until the directory is built.
    entry_count: AtomicUsize,
}

impl PagedDirectory {
    /// A directory with no pages.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pages: [const { AtomicPtr::new(ptr::null_mut()) }; MAXIMUM_PAGE_COUNT],
            entry_count: AtomicUsize::new(0),
        }
    }

    /// Allocates the pages for `entry_count` entries and returns how many frames they took.
    ///
    /// Call once at boot, while the caller still owns the frame allocator.
    /// Frames already taken are not returned on failure, because boot treats that as fatal.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError`] when the directory exists, when `entry_count` is zero or
    /// exceeds [`MAXIMUM_ENTRY_COUNT`], or when the frame allocator runs out of frames.
    pub fn initialise(
        &self,
        frame_allocator: &mut FrameAllocator,
        entry_count: usize,
    ) -> Result<usize, DirectoryError> {
        if self.entry_count.load(Ordering::Acquire) != 0 {
            return Err(DirectoryError::AlreadyInitialised);
        }
        if entry_count == 0 {
            return Err(DirectoryError::Empty);
        }
        if entry_count > MAXIMUM_ENTRY_COUNT {
            return Err(DirectoryError::ExceedsCapacity);
        }

        let page_count = entry_count.div_ceil(ENTRIES_PER_PAGE);

        for page_pointer in self.pages.iter().take(page_count) {
            let frame_address = frame_allocator
                .allocate_frame()
                .ok_or(DirectoryError::OutOfFrames)?;
            let page = physical_to_virtual(frame_address) as *mut AtomicUsize;

            for offset in 0..ENTRIES_PER_PAGE {
                // SAFETY: The frame is exclusively owned here and holds ENTRIES_PER_PAGE entries.
                unsafe { page.add(offset).write(AtomicUsize::new(0)) };
            }

            page_pointer.store(page, Ordering::Release);
        }

        // Publish last, so anyone who sees a nonzero count also sees every page.
        self.entry_count.store(entry_count, Ordering::Release);

        Ok(page_count)
    }

    /// Number of entries in the directory. Zero before [`Self::initialise`] has finished.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entry_count.load(Ordering::Acquire)
    }

    /// The entry cell at `index`, or `None` when it lies outside the directory.
    fn entry(&self, index: usize) -> Option<&AtomicUsize> {
        if index >= self.entry_count() {
            return None;
        }

        let page = self
            .pages
            .get(index / ENTRIES_PER_PAGE)?
            .load(Ordering::Acquire);
        if page.is_null() {
            return None;
        }

        // SAFETY: The index is below the published entry count, so this page was fully
        // initialised before publication. Pages are never freed or moved.
        Some(unsafe { &*page.add(index % ENTRIES_PER_PAGE) })
    }

    /// The address stored at `index`, or `None` when it is unset or out of range.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<usize> {
        match self.entry(index)?.load(Ordering::Acquire) {
            0 => None,
            address => Some(address),
        }
    }

    /// Stores a nonzero `address` at `index`. Returns `false` when the index is out of range.
    pub fn set(&self, index: usize, address: usize) -> bool {
        debug_assert!(address != 0, "Zero marks an unset entry");
        match self.entry(index) {
            Some(entry) => {
                entry.store(address, Ordering::Release);
                true
            }
            None => false,
        }
    }
}
