//! Actor names are short ASCII strings that identify actors in the kernel's global registry. The actor table is the kernel's global registry of actors, and the control block is the per-actor state that the kernel maintains for each actor. Lineage is the parent-child relationship between actors, and lifecycle is the state machine that governs an actor's existence.

use core::fmt;

/// Maximum number of bytes in an actor name.
pub const MAXIMUM_NAME_BYTE_COUNT: usize = 32;

/// System-wide address for the Console actor, can be addressed through the `FindActorByName` system call.
const CONSOLE_NAME: &str = "LocalHost.Console";
/// System-wide address for the File-System actor, can be addressed through the `FindActorByName` system call.
const FILE_SYSTEM_NAME: &str = "LocalHost.FileSystem";
/// System-wide address for the Genesis actor, can be addressed through the `FindActorByName` system call.
const GENESIS_NAME: &str = "LocalHost.Genesis";
/// System-wide address for the Terminal actor, can be addressed through the `FindActorByName` system call.
const TERMINAL_NAME: &str = "LocalHost.Terminal";

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

    /// The name of the Console actor.
    pub const CONSOLE: Self = match Self::new(CONSOLE_NAME) {
        Ok(name) => name,
        Err(_) => panic!("CONSOLE_NAME is not a valid name"),
    };

    /// The name of the File-System actor.
    pub const FILE_SYSTEM: Self = match Self::new(FILE_SYSTEM_NAME) {
        Ok(name) => name,
        Err(_) => panic!("FILE_SYSTEM_NAME is not a valid name"),
    };

    /// The name of the Genesis actor.
    pub const GENESIS: Self = match Self::new(GENESIS_NAME) {
        Ok(name) => name,
        Err(_) => panic!("GENESIS_NAME is not a valid name"),
    };

    /// The name of the Terminal actor.
    pub const TERMINAL: Self = match Self::new(TERMINAL_NAME) {
        Ok(name) => name,
        Err(_) => panic!("TERMINAL_NAME is not a valid name"),
    };

    /// Validates an actor name containing 1..=32 bytes of `[A-Za-z0-9._-]`.
    ///
    /// # Errors
    ///
    /// Returns [`NameError::Empty`] if the name is empty,
    /// [`NameError::TooLong`] if the name exceeds 32 bytes,
    /// or [`NameError::InvalidCharacter`] if it contains unsupported characters.
    pub const fn new(name: &str) -> Result<Self, NameError> {
        let source = name.as_bytes();

        if source.is_empty() {
            return Err(NameError::Empty);
        }

        if source.len() > MAXIMUM_NAME_BYTE_COUNT {
            return Err(NameError::TooLong);
        }

        let mut bytes = [0; MAXIMUM_NAME_BYTE_COUNT];
        let mut remaining_source = source;
        let mut remaining_destination: &mut [u8] = &mut bytes;
        let mut length = 0u8;

        while let Some((&byte, source_tail)) = remaining_source.split_first() {
            if !(byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.') {
                return Err(NameError::InvalidCharacter);
            }

            let Some((destination_byte, destination_tail)) =
                remaining_destination.split_first_mut()
            else {
                return Err(NameError::TooLong);
            };

            *destination_byte = byte;
            remaining_source = source_tail;
            remaining_destination = destination_tail;
            length += 1;
        }

        Ok(Self { bytes, length })
    }

    /// For constants: an invalid name is a compile error.
    #[must_use]
    pub const fn from_static(name: &'static str) -> Self {
        match Self::new(name) {
            Ok(name) => name,
            Err(_) => panic!("invalid actor name"),
        }
    }

    /// Returns true when the name is empty and cannot be looked up.
    #[must_use]
    pub const fn is_unnamed(&self) -> bool {
        self.length == 0
    }

    /// Returns the name as a UTF-8 string slice.
    #[must_use]
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
