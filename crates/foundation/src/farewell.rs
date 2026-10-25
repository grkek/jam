//! What a watcher receives when an actor ends.

use crate::{address::Address, cause::Cause};

/// Largest testimony the kernel will carry, regardless of an actor's resource budget.
pub const TESTIMONY_BYTE_MAXIMUM: usize = 1024;

/// An actor's own last words: bounded, opaque to the kernel, and authored by the actor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Testimony {
    length: usize,
    bytes: [u8; TESTIMONY_BYTE_MAXIMUM],
}

impl Testimony {
    /// Testimony with nothing in it.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            length: 0,
            bytes: [0; TESTIMONY_BYTE_MAXIMUM],
        }
    }

    /// Copies as much of `source_bytes` as fits, discarding the rest.
    #[must_use]
    pub fn from_bytes(source_bytes: &[u8]) -> Self {
        let mut bytes = [0_u8; TESTIMONY_BYTE_MAXIMUM];
        let mut length = 0;
        for (destination, source) in bytes.iter_mut().zip(source_bytes) {
            *destination = *source;
            length += 1;
        }
        Self { length, bytes }
    }

    /// The bytes the actor left behind.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.length).unwrap_or(&[])
    }
}

impl Default for Testimony {
    fn default() -> Self {
        Self::empty()
    }
}

/// Delivered to every watcher when an actor ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Farewell {
    /// The actor that ended.
    pub address: Address,
    /// Why it ended, as established by the kernel.
    pub cause: Cause,
    /// What the actor said about itself, if anything.
    pub testimony: Testimony,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::LocalAddress;

    #[test]
    fn short_testimony_is_kept_whole() {
        let testimony = Testimony::from_bytes(b"out of cheese");
        assert_eq!(testimony.as_bytes(), b"out of cheese");
    }

    #[test]
    fn oversized_testimony_is_truncated_to_the_maximum() {
        let oversized = [7_u8; TESTIMONY_BYTE_MAXIMUM * 2];
        let testimony = Testimony::from_bytes(&oversized);
        assert_eq!(testimony.as_bytes().len(), TESTIMONY_BYTE_MAXIMUM);
    }

    #[test]
    fn empty_testimony_has_no_bytes() {
        assert!(Testimony::default().as_bytes().is_empty());
    }

    #[test]
    fn farewell_carries_address_cause_and_testimony() {
        let address = Address::Local(LocalAddress::new(2, 5));
        let farewell = Farewell {
            address,
            cause: Cause::Panicked,
            testimony: Testimony::from_bytes(b"index out of range"),
        };
        assert_eq!(farewell.address, address);
        assert_eq!(farewell.cause, Cause::Panicked);
        assert_eq!(farewell.testimony.as_bytes(), b"index out of range");
    }
}
