//! Tracks which physical frames are free, one bit per frame.

const BITS_PER_WORD: usize = 64;

/// Why a frame could not be freed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFreeError {
    /// The frame index lies beyond the frames this bitmap tracks.
    OutOfRange,
    /// The frame was not allocated, so freeing it again would corrupt the bookkeeping.
    NotAllocated,
}

/// A bitmap over physical frames. A set bit means the frame is in use.
pub struct FrameBitmap<'storage> {
    storage: &'storage mut [u64],
    frame_count: usize,
    free_frame_count: usize,
    search_start_word: usize,
}

impl<'storage> FrameBitmap<'storage> {
    /// Words of storage needed to track `frame_count` frames.
    #[must_use]
    pub const fn required_storage_word_count(frame_count: usize) -> usize {
        frame_count.div_ceil(BITS_PER_WORD)
    }

    /// Starts tracking `frame_count` frames, all free. Returns `None` when `storage` is too small.
    pub fn new(storage: &'storage mut [u64], frame_count: usize) -> Option<Self> {
        let word_count = Self::required_storage_word_count(frame_count);
        let storage = storage.get_mut(..word_count)?;
        storage.fill(0);
        let mut bitmap = Self {
            storage,
            frame_count,
            free_frame_count: frame_count,
            search_start_word: 0,
        };
        // Bits past the last frame stay set, so they can never be handed out.
        for padding_index in frame_count..word_count * BITS_PER_WORD {
            bitmap.write_bit(padding_index, true);
        }
        Some(bitmap)
    }

    /// Frames this bitmap tracks.
    #[must_use]
    pub const fn frame_count(&self) -> usize {
        self.frame_count
    }

    /// Frames currently free.
    #[must_use]
    pub const fn free_frame_count(&self) -> usize {
        self.free_frame_count
    }

    /// Marks a range of frames as in use, for memory that is already taken.
    pub fn mark_used_range(&mut self, first_frame_index: usize, frame_count: usize) {
        let end_frame_index = first_frame_index
            .saturating_add(frame_count)
            .min(self.frame_count);
        for frame_index in first_frame_index..end_frame_index {
            if self.write_bit(frame_index, true) == Some(false) {
                self.free_frame_count -= 1;
            }
        }
    }

    /// Takes a free frame and returns its index, or `None` when none are left.
    pub fn allocate(&mut self) -> Option<usize> {
        let word_count = self.storage.len();
        if word_count == 0 {
            return None;
        }
        for step in 0..word_count {
            let word_index = (self.search_start_word + step) % word_count;
            let word = self.storage.get_mut(word_index)?;
            if *word != u64::MAX {
                let bit_index = (!*word).trailing_zeros() as usize;
                *word |= 1_u64 << bit_index;
                self.free_frame_count -= 1;
                self.search_start_word = word_index;
                return Some(word_index * BITS_PER_WORD + bit_index);
            }
        }
        None
    }

    /// Returns a frame to the free set.
    ///
    /// # Errors
    ///
    /// Fails when the index is out of range or the frame was not allocated.
    pub fn free(&mut self, frame_index: usize) -> Result<(), FrameFreeError> {
        if frame_index >= self.frame_count {
            return Err(FrameFreeError::OutOfRange);
        }
        match self.write_bit(frame_index, false) {
            Some(true) => {
                self.free_frame_count += 1;
                self.search_start_word = frame_index / BITS_PER_WORD;
                Ok(())
            }
            Some(false) => Err(FrameFreeError::NotAllocated),
            None => Err(FrameFreeError::OutOfRange),
        }
    }

    /// Sets or clears one bit and returns its previous value.
    fn write_bit(&mut self, frame_index: usize, used: bool) -> Option<bool> {
        let word = self.storage.get_mut(frame_index / BITS_PER_WORD)?;
        let mask = 1_u64 << (frame_index % BITS_PER_WORD);
        let previous = *word & mask != 0;
        if used {
            *word |= mask;
        } else {
            *word &= !mask;
        }
        Some(previous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACKED_FRAME_COUNT: usize = 70;

    #[test]
    fn allocates_distinct_frames() {
        let mut storage = [0_u64; 2];
        let mut bitmap = FrameBitmap::new(&mut storage, TRACKED_FRAME_COUNT).unwrap();
        let first = bitmap.allocate().unwrap();
        let second = bitmap.allocate().unwrap();
        assert_ne!(first, second);
        assert_eq!(bitmap.free_frame_count(), TRACKED_FRAME_COUNT - 2);
    }

    #[test]
    fn hands_out_exactly_the_tracked_frames() {
        let mut storage = [0_u64; 2];
        let mut bitmap = FrameBitmap::new(&mut storage, TRACKED_FRAME_COUNT).unwrap();
        for _ in 0..TRACKED_FRAME_COUNT {
            assert!(bitmap.allocate().is_some());
        }
        assert_eq!(bitmap.allocate(), None);
        assert_eq!(bitmap.free_frame_count(), 0);
    }

    #[test]
    fn a_freed_frame_can_be_allocated_again() {
        let mut storage = [0_u64; 2];
        let mut bitmap = FrameBitmap::new(&mut storage, TRACKED_FRAME_COUNT).unwrap();
        let frame_index = bitmap.allocate().unwrap();
        assert_eq!(bitmap.free(frame_index), Ok(()));
        assert_eq!(bitmap.allocate(), Some(frame_index));
    }

    #[test]
    fn freeing_twice_is_rejected() {
        let mut storage = [0_u64; 2];
        let mut bitmap = FrameBitmap::new(&mut storage, TRACKED_FRAME_COUNT).unwrap();
        let frame_index = bitmap.allocate().unwrap();
        assert_eq!(bitmap.free(frame_index), Ok(()));
        assert_eq!(bitmap.free(frame_index), Err(FrameFreeError::NotAllocated));
    }

    #[test]
    fn freeing_out_of_range_is_rejected() {
        let mut storage = [0_u64; 2];
        let mut bitmap = FrameBitmap::new(&mut storage, TRACKED_FRAME_COUNT).unwrap();
        assert_eq!(
            bitmap.free(TRACKED_FRAME_COUNT),
            Err(FrameFreeError::OutOfRange)
        );
    }

    #[test]
    fn reserved_frames_are_never_allocated() {
        let mut storage = [0_u64; 2];
        let mut bitmap = FrameBitmap::new(&mut storage, TRACKED_FRAME_COUNT).unwrap();
        bitmap.mark_used_range(0, 10);
        assert_eq!(bitmap.free_frame_count(), TRACKED_FRAME_COUNT - 10);
        assert_eq!(bitmap.allocate(), Some(10));
    }

    #[test]
    fn storage_that_is_too_small_is_refused() {
        let mut storage = [0_u64; 1];
        assert!(FrameBitmap::new(&mut storage, TRACKED_FRAME_COUNT).is_none());
    }
}
