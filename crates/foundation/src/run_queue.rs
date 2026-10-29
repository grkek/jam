//! A bounded, allocation-free FIFO queue of actor addresses for scheduling.
//!
//! Entries are [`LocalAddress`] values, containing both the actor table slot
//! and its generation. An entry can therefore be rejected if the actor was
//! terminated and the table slot has since been recycled. This is a scheduler
//! run queue, not an actor message inbox: it deliberately remains FIFO.

use crate::address::LocalAddress;

/// Holds up to `CAPACITY` runnable actor identities without allocating.
pub struct RunQueue<const CAPACITY: usize> {
    addresses: [LocalAddress; CAPACITY],
    head: usize,
    length: usize,
}

impl<const CAPACITY: usize> RunQueue<CAPACITY> {
    /// Constructs an empty run queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            addresses: [LocalAddress::new(0, 0); CAPACITY],
            head: 0,
            length: 0,
        }
    }

    /// Returns the number of waiting actors.
    #[must_use]
    pub const fn length(&self) -> usize {
        self.length
    }

    /// Appends one actor incarnation to the queue.
    ///
    /// Returns `false` without modifying the queue when it is full.
    pub fn push_back(&mut self, address: LocalAddress) -> bool {
        if self.length == CAPACITY {
            return false;
        }
        let tail = (self.head + self.length) % CAPACITY;
        let Some(cell) = self.addresses.get_mut(tail) else {
            return false;
        };
        *cell = address;
        self.length += 1;
        true
    }

    /// Removes the oldest queued actor incarnation, if one exists.
    pub fn pop_front(&mut self) -> Option<LocalAddress> {
        if self.length == 0 {
            return None;
        }
        let address = *self.addresses.get(self.head)?;
        self.head = (self.head + 1) % CAPACITY;
        self.length -= 1;
        Some(address)
    }

    /// Removes the newest queued actor incarnation for work stealing.
    pub fn pop_back(&mut self) -> Option<LocalAddress> {
        if self.length == 0 {
            return None;
        }
        let tail = (self.head + self.length - 1) % CAPACITY;
        let address = *self.addresses.get(tail)?;
        self.length -= 1;
        Some(address)
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

    fn address(slot_index: u32, generation: u32) -> LocalAddress {
        LocalAddress::new(slot_index, generation)
    }

    #[test]
    fn serves_the_oldest_first() {
        let mut queue = RunQueue::<4>::new();
        assert!(queue.push_back(address(1, 1)));
        assert!(queue.push_back(address(2, 1)));
        assert_eq!(queue.pop_front(), Some(address(1, 1)));
        assert_eq!(queue.pop_front(), Some(address(2, 1)));
        assert_eq!(queue.pop_front(), None);
    }

    #[test]
    fn stealing_takes_the_newest() {
        let mut queue = RunQueue::<4>::new();
        assert!(queue.push_back(address(1, 2)));
        assert!(queue.push_back(address(2, 3)));
        assert_eq!(queue.pop_back(), Some(address(2, 3)));
        assert_eq!(queue.pop_front(), Some(address(1, 2)));
    }

    #[test]
    fn a_full_queue_refuses_more() {
        let mut queue = RunQueue::<2>::new();
        assert!(queue.push_back(address(1, 1)));
        assert!(queue.push_back(address(2, 1)));
        assert!(!queue.push_back(address(3, 1)));
        assert_eq!(queue.length(), 2);
    }

    #[test]
    fn order_survives_wrapping_around() {
        let mut queue = RunQueue::<4>::new();
        for slot_index in 1..=4 {
            assert!(queue.push_back(address(slot_index, 5)));
        }
        assert_eq!(queue.pop_front(), Some(address(1, 5)));
        assert_eq!(queue.pop_front(), Some(address(2, 5)));
        assert!(queue.push_back(address(5, 5)));
        assert!(queue.push_back(address(6, 5)));
        for slot_index in 3..=6 {
            assert_eq!(queue.pop_front(), Some(address(slot_index, 5)));
        }
    }

    #[test]
    fn identity_survives_slot_reuse() {
        let mut queue = RunQueue::<2>::new();
        assert!(queue.push_back(address(7, 4)));
        assert!(queue.push_back(address(7, 5)));
        assert_eq!(queue.pop_front(), Some(address(7, 4)));
        assert_eq!(queue.pop_front(), Some(address(7, 5)));
    }

    #[test]
    fn zero_capacity_is_safe() {
        let mut queue = RunQueue::<0>::new();
        assert!(!queue.push_back(address(1, 1)));
        assert_eq!(queue.pop_front(), None);
        assert_eq!(queue.pop_back(), None);
    }
}
