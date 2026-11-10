//! A message tag: protocol in the high half, operation in the low half.

/// Identifies one message type. Any `u64` is a valid tag; whether it means
/// something is up to the receiver, so unknown tags fall through a `match`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Tag(u64);

impl Tag {
    /// Builds a tag. Wire value is `protocol << 16 | operation`.
    #[must_use]
    pub const fn new(protocol: u16, operation: u16) -> Self {
        Self(((protocol as u64) << 16) | operation as u64)
    }

    /// Wraps a value taken from message metadata.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The value placed in message metadata.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The protocol this tag belongs to.
    #[must_use]
    pub const fn protocol(self) -> u16 {
        #[allow(clippy::cast_possible_truncation)]
        {
            (self.0 >> 16) as u16
        }
    }
}
