//! A double-ended queue of runnable actors, sized at boot.
//!
//! The entries live in frames behind a paged directory, so the capacity follows the actor
//! table and never a compile-time ceiling. The queue itself is not synchronised: each core
//! keeps its queue behind a lock.

use foundation::address::LocalAddress;

use crate::{
    memory::{
        frame_allocator::FrameAllocator,
        paged_directory::{DirectoryError, PagedDirectory},
    },
    system_calls::actor_calls::pack_local_address,
};

/// Decodes an address stored in the queue.
fn unpack_address(packed: u64) -> LocalAddress {
    let bytes = packed.to_le_bytes();
    let (slot, generation) = bytes.split_at(4);
    LocalAddress::new(
        u32::from_le_bytes(slot.try_into().unwrap_or([0; 4])),
        u32::from_le_bytes(generation.try_into().unwrap_or([0; 4])),
    )
}

/// Runnable actors waiting in order, oldest first.
pub struct Queue {
    /// Packed addresses. A live address is never zero, because generations start at one,
    /// and the directory reserves zero for "unset".
    entries: PagedDirectory,
    capacity: usize,
    head: usize,
    length: usize,
}

impl Queue {
    /// A queue with no storage, which refuses every push until it is initialised.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: PagedDirectory::new(),
            capacity: 0,
            head: 0,
            length: 0,
        }
    }

    /// Allocates storage for `capacity` entries and returns how many frames it took.
    ///
    /// Call once at boot, while the caller still owns the frame allocator.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError`] when the queue already has storage, when `capacity` is zero
    /// or too large, or when the frame allocator runs out of frames.
    pub fn initialise(
        &mut self,
        frame_allocator: &mut FrameAllocator,
        capacity: usize,
    ) -> Result<usize, DirectoryError> {
        let frame_count = self.entries.initialise(frame_allocator, capacity)?;
        self.capacity = capacity;
        Ok(frame_count)
    }

    /// Number of entries the queue can hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of entries waiting.
    #[must_use]
    pub const fn length(&self) -> usize {
        self.length
    }

    /// Whether no entry is waiting.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Adds `address` at the back. Returns `false` when the queue is full.
    pub fn push_back(&mut self, address: LocalAddress) -> bool {
        if self.length == self.capacity {
            return false;
        }

        let packed = pack_local_address(address);
        let Ok(stored) = usize::try_from(packed) else {
            return false;
        };
        if stored == 0 {
            return false;
        }

        let index = (self.head + self.length) % self.capacity;
        if !self.entries.set(index, stored) {
            return false;
        }

        self.length += 1;
        true
    }

    /// Removes and returns the oldest address.
    pub fn pop_front(&mut self) -> Option<LocalAddress> {
        if self.length == 0 {
            return None;
        }

        let stored = self.entries.get(self.head)?;
        self.head = (self.head + 1) % self.capacity;
        self.length -= 1;
        Some(unpack_address(stored as u64))
    }

    /// Removes and returns the newest address, so a thief leaves the owner's oldest work alone.
    pub fn pop_back(&mut self) -> Option<LocalAddress> {
        if self.length == 0 {
            return None;
        }

        let index = (self.head + self.length - 1) % self.capacity;
        let stored = self.entries.get(index)?;
        self.length -= 1;
        Some(unpack_address(stored as u64))
    }
}
