//! A fixed-capacity first-in first-out queue of actor slot indices.

/// Holds up to `CAPACITY` slot indices without allocating.
pub struct RunQueue<const CAPACITY: usize> {
    slot_indices: [usize; CAPACITY],
    head: usize,
    length: usize,
}

impl<const CAPACITY: usize> RunQueue<CAPACITY> {
    /// An empty queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slot_indices: [0; CAPACITY],
            head: 0,
            length: 0,
        }
    }

    /// Number of slot indices waiting.
    #[must_use]
    pub const fn length(&self) -> usize {
        self.length
    }

    /// Adds a slot index at the back. Returns `false` when the queue is full.
    pub fn push_back(&mut self, slot_index: usize) -> bool {
        if self.length == CAPACITY {
            return false;
        }
        let tail = (self.head + self.length) % CAPACITY;
        let Some(cell) = self.slot_indices.get_mut(tail) else {
            return false;
        };
        *cell = slot_index;
        self.length += 1;
        true
    }

    /// Removes the slot index that has waited longest.
    pub fn pop_front(&mut self) -> Option<usize> {
        if self.length == 0 {
            return None;
        }
        let slot_index = *self.slot_indices.get(self.head)?;
        self.head = (self.head + 1) % CAPACITY;
        self.length -= 1;
        Some(slot_index)
    }

    /// Removes the slot index that arrived last. Other cores steal from this end,
    /// so they leave the owner's oldest work alone.
    pub fn pop_back(&mut self) -> Option<usize> {
        if self.length == 0 {
            return None;
        }
        let tail = (self.head + self.length - 1) % CAPACITY;
        let slot_index = *self.slot_indices.get(tail)?;
        self.length -= 1;
        Some(slot_index)
    }
}

impl<const CAPACITY: usize> Default for RunQueue<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_the_oldest_first() {
        let mut queue = RunQueue::<4>::new();
        queue.push_back(1);
        queue.push_back(2);
        assert_eq!(queue.pop_front(), Some(1));
        assert_eq!(queue.pop_front(), Some(2));
        assert_eq!(queue.pop_front(), None);
    }

    #[test]
    fn stealing_takes_the_newest() {
        let mut queue = RunQueue::<4>::new();
        queue.push_back(1);
        queue.push_back(2);
        assert_eq!(queue.pop_back(), Some(2));
        assert_eq!(queue.pop_front(), Some(1));
    }

    #[test]
    fn a_full_queue_refuses_more() {
        let mut queue = RunQueue::<2>::new();
        assert!(queue.push_back(1));
        assert!(queue.push_back(2));
        assert!(!queue.push_back(3));
        assert_eq!(queue.length(), 2);
    }

    #[test]
    fn order_survives_wrapping_around() {
        let mut queue = RunQueue::<4>::new();
        for slot_index in 1..=4 {
            queue.push_back(slot_index);
        }
        assert_eq!(queue.pop_front(), Some(1));
        assert_eq!(queue.pop_front(), Some(2));
        queue.push_back(5);
        queue.push_back(6);
        for expected in 3..=6 {
            assert_eq!(queue.pop_front(), Some(expected));
        }
    }
}
