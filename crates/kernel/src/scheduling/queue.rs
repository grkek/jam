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
    LocalAddress::new(packed as u32, (packed >> 32) as u32)
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

    /// Computes a ring-buffer index without integer division.
    ///
    /// Requires a nonempty queue allocation and an offset below its capacity.
    fn ring_index(&self, offset: usize) -> usize {
        debug_assert!(self.capacity != 0 && offset < self.capacity);
        let remaining = self.capacity - self.head;
        if offset >= remaining {
            offset - remaining
        } else {
            self.head + offset
        }
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

        let index = self.ring_index(self.length);
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
        self.head += 1;
        if self.head == self.capacity {
            self.head = 0;
        }
        self.length -= 1;
        Some(unpack_address(stored as u64))
    }

    /// Removes and returns the newest address, so a thief leaves the owner's oldest work alone.
    pub fn pop_back(&mut self) -> Option<LocalAddress> {
        if self.length == 0 {
            return None;
        }

        let index = self.ring_index(self.length - 1);
        let stored = self.entries.get(index)?;
        self.length -= 1;
        Some(unpack_address(stored as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_index_wraps_without_division() {
        let queue = Queue {
            entries: PagedDirectory::new(),
            capacity: 7,
            head: 5,
            length: 0,
        };
        assert_eq!(queue.ring_index(0), 5);
        assert_eq!(queue.ring_index(1), 6);
        assert_eq!(queue.ring_index(2), 0);
        assert_eq!(queue.ring_index(6), 4);
    }
}
