//! Capability handles into the file system.

/// Reference to a node as one client holds it, in the style of a 9P fid.
///
/// The generation counter changes every time a handle slot is reused, so a stale handle
/// can never reach the reference that now occupies the same slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileSystemHandle {
    slot_index: u32,
    generation_counter: u32,
}

impl FileSystemHandle {
    /// Builds a handle from a slot and its generation. The file system validates it on use.
    #[must_use]
    pub const fn new(slot_index: u32, generation_counter: u32) -> Self {
        Self {
            slot_index,
            generation_counter,
        }
    }

    /// Index of the handle's slot in the handle table.
    #[must_use]
    pub const fn slot_index(self) -> u32 {
        self.slot_index
    }

    /// Generation of the slot this handle was issued for.
    #[must_use]
    pub const fn generation_counter(self) -> u32 {
        self.generation_counter
    }

    /// Slot in the low half, generation in the high half.
    #[must_use]
    pub fn to_packed(self) -> u64 {
        u64::from(self.slot_index()) | (u64::from(self.generation_counter()) << 32)
    }

    /// Inverse of [`Self::to_packed`].
    #[must_use]
    pub fn from_packed(packed: u64) -> Self {
        let [s0, s1, s2, s3, g0, g1, g2, g3] = packed.to_le_bytes();
        Self::new(
            u32::from_le_bytes([s0, s1, s2, s3]),
            u32::from_le_bytes([g0, g1, g2, g3]),
        )
    }
}

impl core::fmt::Display for FileSystemHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "<fs.{}.{}>", self.slot_index, self.generation_counter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_remembers_slot_and_generation() {
        let handle = FileSystemHandle::new(7, 3);
        assert_eq!(handle.slot_index(), 7);
        assert_eq!(handle.generation_counter(), 3);
    }

    #[test]
    fn same_slot_with_different_generation_is_a_different_handle() {
        assert_ne!(FileSystemHandle::new(7, 3), FileSystemHandle::new(7, 4));
    }

    #[test]
    fn handle_round_trips_through_packed_form() {
        let handle = FileSystemHandle::new(12, 34);
        assert_eq!(handle.to_packed(), 12 | (34_u64 << 32));
        assert_eq!(FileSystemHandle::from_packed(handle.to_packed()), handle);
    }

    #[test]
    fn handle_display_format() {
        assert_eq!(
            std::format!("{}", FileSystemHandle::new(5, 12)),
            "<fs.5.12>"
        );
    }
}
