//! Handles to Scrolls: sealed, immutable, shared byte buffers.

/// Bytes in the serialized form of a scroll reference.
pub const SCROLL_REFERENCE_WIRE_BYTE_COUNT: usize = 8;

/// Most Scroll references one actor may hold at once.
///
/// This bounds the kernel's bookkeeping, not memory, so it is a fixed system constant
/// and not part of an actor's resource budget.
pub const SCROLL_REFERENCE_MAXIMUM_PER_ACTOR: usize = 256;
/// Most unsealed draft Scrolls one actor may hold at once.
pub const SCROLL_DRAFT_MAXIMUM_PER_ACTOR: usize = 8;

/// An actor's handle to a Scroll.
///
/// It indexes the holder's own reference table in the kernel, so it means nothing
/// to any other actor. When a reference travels in a message, the kernel replaces it
/// with a fresh reference valid in the receiver's table. The generation counter
/// makes a stale handle harmless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScrollReference {
    table_index: u32,
    generation_counter: u32,
}

impl ScrollReference {
    /// Builds a reference from a table slot and its generation. The kernel validates it on use.
    #[must_use]
    pub const fn new(table_index: u32, generation_counter: u32) -> Self {
        Self {
            table_index,
            generation_counter,
        }
    }

    /// Index into the holder's reference table.
    #[must_use]
    pub const fn table_index(self) -> u32 {
        self.table_index
    }

    /// Generation of the table slot this reference was issued for.
    #[must_use]
    pub const fn generation_counter(self) -> u32 {
        self.generation_counter
    }

    /// Serializes the reference, little-endian.
    #[must_use]
    pub fn to_wire_bytes(self) -> [u8; SCROLL_REFERENCE_WIRE_BYTE_COUNT] {
        crate::wire_bytes::collect_array(
            self.table_index
                .to_le_bytes()
                .into_iter()
                .chain(self.generation_counter.to_le_bytes()),
        )
    }

    /// Reads a reference serialized by `to_wire_bytes`.
    #[must_use]
    pub fn from_wire_bytes(wire_bytes: [u8; SCROLL_REFERENCE_WIRE_BYTE_COUNT]) -> Self {
        let [
            index_0,
            index_1,
            index_2,
            index_3,
            generation_0,
            generation_1,
            generation_2,
            generation_3,
        ] = wire_bytes;
        Self {
            table_index: u32::from_le_bytes([index_0, index_1, index_2, index_3]),
            generation_counter: u32::from_le_bytes([
                generation_0,
                generation_1,
                generation_2,
                generation_3,
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_remembers_index_and_generation() {
        let reference = ScrollReference::new(4, 9);
        assert_eq!(reference.table_index(), 4);
        assert_eq!(reference.generation_counter(), 9);
    }

    #[test]
    fn reference_round_trips_through_wire_bytes() {
        let reference = ScrollReference::new(123_456, 789);
        assert_eq!(
            ScrollReference::from_wire_bytes(reference.to_wire_bytes()),
            reference
        );
    }
}
