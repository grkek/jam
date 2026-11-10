//! Actor names are short ASCII strings that identify actors in the kernel's global registry. The actor table is the kernel's global registry of actors, and the control block is the per-actor state that the kernel maintains for each actor. Lineage is the parent-child relationship between actors, and lifecycle is the state machine that governs an actor's existence.

use core::fmt;

/// The maximum number of bytes in an actor name, including the null terminator.
pub const MAXIMUM_NAME_BYTE_COUNT: usize = 32;

/// The name of an actor that cannot be looked up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    /// The name is empty and cannot be looked up.
    Empty,
    /// The name is longer than 32 bytes.
    TooLong,
    /// The name contains a non-ASCII or non-alphanumeric character.
    InvalidCharacter,
}

/// A short ASCII actor name such as `terminal` or `console-driver`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Name {
    bytes: [u8; MAXIMUM_NAME_BYTE_COUNT],
    length: u8,
}

impl Name {
    /// The name of an actor that cannot be looked up.
    pub const UNNAMED: Self = Self {
        bytes: [0; MAXIMUM_NAME_BYTE_COUNT],
        length: 0,
    };

    /// Validates `name`: 1..=32 bytes of `[A-Za-z0-9._-]`.
    pub const fn new(name: &str) -> Result<Self, NameError> {
        let source = name.as_bytes();
        if source.is_empty() {
            return Err(NameError::Empty);
        }
        if source.len() > MAXIMUM_NAME_BYTE_COUNT {
            return Err(NameError::TooLong);
        }
        let mut bytes = [0; MAXIMUM_NAME_BYTE_COUNT];
        let mut index = 0;
        while index < source.len() {
            let byte = source[index];
            if !(byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.') {
                return Err(NameError::InvalidCharacter);
            }
            bytes[index] = byte;
            index += 1;
        }
        Ok(Self {
            bytes,
            length: source.len() as u8,
        })
    }

    /// For constants: an invalid name is a compile error.
    pub const fn from_static(name: &'static str) -> Self {
        match Self::new(name) {
            Ok(name) => name,
            Err(_) => panic!("invalid actor name"),
        }
    }

    /// Returns true when the name is empty and cannot be looked up.
    pub const fn is_unnamed(&self) -> bool {
        self.length == 0
    }

    /// Returns the name as a UTF-8 string slice.
    pub fn as_str(&self) -> &str {
        self.bytes
            .get(..usize::from(self.length))
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or("")
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self.as_str())
    }
}
