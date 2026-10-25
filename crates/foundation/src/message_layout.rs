//! Wire layout of a message: a header, payload bytes, and slots.
//!
//! The kernel never parses the payload. Everything the kernel must validate or rewrite
//! travels in separate slots: an address, or a reference to a Scroll. The payload refers
//! to a slot by its number. Data too large for the payload travels in a Scroll.

use crate::{
    address::{ADDRESS_WIRE_BYTE_COUNT, Address, AddressWireError},
    scroll::{SCROLL_REFERENCE_WIRE_BYTE_COUNT, ScrollReference},
    wire_bytes::{collect_array, read_array},
};

/// Largest payload a message may carry, in bytes.
pub const MESSAGE_PAYLOAD_BYTE_MAXIMUM: usize = 4096;
/// Most slots a single message may carry.
pub const MESSAGE_SLOT_MAXIMUM: usize = 8;
/// Bytes in a serialized slot. Every slot has the same size.
pub const SLOT_WIRE_BYTE_COUNT: usize = ADDRESS_WIRE_BYTE_COUNT;
/// Bytes in a serialized header: sender address, payload byte count, slot count.
pub const MESSAGE_HEADER_WIRE_BYTE_COUNT: usize = ADDRESS_WIRE_BYTE_COUNT + 4 + 4;

/// Kind number of a scroll slot. Kinds 0 and 1 belong to addresses.
const SCROLL_SLOT_KIND_NUMBER: u8 = 2;
const SCROLL_SLOT_PADDING_BYTE_COUNT: usize =
    SLOT_WIRE_BYTE_COUNT - 1 - SCROLL_REFERENCE_WIRE_BYTE_COUNT;

/// Something the kernel validates and rewrites as a message crosses between actors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The address of an actor.
    Address(Address),
    /// A reference to a Scroll.
    Scroll(ScrollReference),
}

/// Why bytes could not be read as a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotWireError {
    /// There were fewer bytes than a slot needs.
    Truncated,
    /// The slot holds an address that could not be read.
    MalformedAddress(AddressWireError),
    /// A scroll slot had non-zero padding, so it has two encodings.
    NonCanonicalScrollSlot,
}

impl Slot {
    /// Serializes the slot. Unused bytes are zero.
    #[must_use]
    pub fn to_wire_bytes(self) -> [u8; SLOT_WIRE_BYTE_COUNT] {
        match self {
            Self::Address(address) => address.to_wire_bytes(),
            Self::Scroll(scroll_reference) => collect_array(
                core::iter::once(SCROLL_SLOT_KIND_NUMBER).chain(scroll_reference.to_wire_bytes()),
            ),
        }
    }

    /// Reads a slot from the start of `wire_bytes`. Trailing bytes are ignored.
    ///
    /// # Errors
    ///
    /// Fails when the input is too short, holds a malformed address, or encodes a
    /// scroll slot non-canonically.
    pub fn from_wire_bytes(wire_bytes: &[u8]) -> Result<Self, SlotWireError> {
        let kind_number = *wire_bytes.first().ok_or(SlotWireError::Truncated)?;
        if kind_number != SCROLL_SLOT_KIND_NUMBER {
            return Address::from_wire_bytes(wire_bytes)
                .map(Self::Address)
                .map_err(SlotWireError::MalformedAddress);
        }

        let reference_bytes: [u8; SCROLL_REFERENCE_WIRE_BYTE_COUNT] =
            read_array(wire_bytes, 1).ok_or(SlotWireError::Truncated)?;
        let padding_bytes: [u8; SCROLL_SLOT_PADDING_BYTE_COUNT] =
            read_array(wire_bytes, 1 + SCROLL_REFERENCE_WIRE_BYTE_COUNT)
                .ok_or(SlotWireError::Truncated)?;
        if padding_bytes.iter().any(|padding_byte| *padding_byte != 0) {
            return Err(SlotWireError::NonCanonicalScrollSlot);
        }
        Ok(Self::Scroll(ScrollReference::from_wire_bytes(
            reference_bytes,
        )))
    }
}

/// Why a message layout was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageLayoutError {
    /// The header is shorter than a header must be.
    Truncated,
    /// The payload exceeds `MESSAGE_PAYLOAD_BYTE_MAXIMUM`.
    PayloadTooLarge {
        /// The payload size that was requested.
        requested_byte_count: usize,
    },
    /// More slots than `MESSAGE_SLOT_MAXIMUM`.
    TooManySlots {
        /// The number of slots that was requested.
        requested_slot_count: usize,
    },
    /// The sender address could not be read.
    MalformedSenderAddress(AddressWireError),
}

/// Describes a message. The kernel stamps `sender_address`, so a sender cannot forge it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageHeader {
    sender_address: Address,
    payload_byte_count: u32,
    slot_count: u32,
}

impl MessageHeader {
    /// Builds a header, checking the layout against the maxima.
    ///
    /// # Errors
    ///
    /// Fails when the payload or the number of slots exceeds its maximum.
    pub fn new(
        sender_address: Address,
        payload_byte_count: usize,
        slot_count: usize,
    ) -> Result<Self, MessageLayoutError> {
        let too_large = MessageLayoutError::PayloadTooLarge {
            requested_byte_count: payload_byte_count,
        };
        let too_many = MessageLayoutError::TooManySlots {
            requested_slot_count: slot_count,
        };
        if payload_byte_count > MESSAGE_PAYLOAD_BYTE_MAXIMUM {
            return Err(too_large);
        }
        if slot_count > MESSAGE_SLOT_MAXIMUM {
            return Err(too_many);
        }
        Ok(Self {
            sender_address,
            payload_byte_count: u32::try_from(payload_byte_count).map_err(|_| too_large)?,
            slot_count: u32::try_from(slot_count).map_err(|_| too_many)?,
        })
    }

    /// The actor that sent the message.
    #[must_use]
    pub const fn sender_address(self) -> Address {
        self.sender_address
    }

    /// Bytes of payload that follow the header.
    #[must_use]
    pub const fn payload_byte_count(self) -> usize {
        self.payload_byte_count as usize
    }

    /// Slots that follow the payload.
    #[must_use]
    pub const fn slot_count(self) -> usize {
        self.slot_count as usize
    }

    /// Size of the whole message on the wire: header, payload, and slots.
    #[must_use]
    pub const fn total_byte_count(self) -> usize {
        MESSAGE_HEADER_WIRE_BYTE_COUNT
            + self.payload_byte_count()
            + self.slot_count() * SLOT_WIRE_BYTE_COUNT
    }

    /// Serializes the header.
    #[must_use]
    pub fn to_wire_bytes(self) -> [u8; MESSAGE_HEADER_WIRE_BYTE_COUNT] {
        collect_array(
            self.sender_address
                .to_wire_bytes()
                .into_iter()
                .chain(self.payload_byte_count.to_le_bytes())
                .chain(self.slot_count.to_le_bytes()),
        )
    }

    /// Reads a header from the start of `wire_bytes`, checking the layout.
    ///
    /// # Errors
    ///
    /// Fails when the input is too short, the sender address is malformed, or the
    /// counts exceed their maxima.
    pub fn from_wire_bytes(wire_bytes: &[u8]) -> Result<Self, MessageLayoutError> {
        let sender_address = Address::from_wire_bytes(wire_bytes)
            .map_err(MessageLayoutError::MalformedSenderAddress)?;
        let payload_count_bytes: [u8; 4] =
            read_array(wire_bytes, ADDRESS_WIRE_BYTE_COUNT).ok_or(MessageLayoutError::Truncated)?;
        let slot_count_bytes: [u8; 4] = read_array(wire_bytes, ADDRESS_WIRE_BYTE_COUNT + 4)
            .ok_or(MessageLayoutError::Truncated)?;

        Self::new(
            sender_address,
            u32::from_le_bytes(payload_count_bytes) as usize,
            u32::from_le_bytes(slot_count_bytes) as usize,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::LocalAddress;

    fn sender() -> Address {
        Address::Local(LocalAddress::new(3, 1))
    }

    #[test]
    fn maximum_layout_is_accepted() {
        let header =
            MessageHeader::new(sender(), MESSAGE_PAYLOAD_BYTE_MAXIMUM, MESSAGE_SLOT_MAXIMUM);
        assert!(header.is_ok());
    }

    #[test]
    fn oversized_payload_is_rejected() {
        assert_eq!(
            MessageHeader::new(sender(), MESSAGE_PAYLOAD_BYTE_MAXIMUM + 1, 0),
            Err(MessageLayoutError::PayloadTooLarge {
                requested_byte_count: MESSAGE_PAYLOAD_BYTE_MAXIMUM + 1
            })
        );
    }

    #[test]
    fn too_many_slots_are_rejected() {
        assert_eq!(
            MessageHeader::new(sender(), 0, MESSAGE_SLOT_MAXIMUM + 1),
            Err(MessageLayoutError::TooManySlots {
                requested_slot_count: MESSAGE_SLOT_MAXIMUM + 1
            })
        );
    }

    #[test]
    fn total_byte_count_adds_header_payload_and_slots() {
        let header = MessageHeader::new(sender(), 100, 2).unwrap();
        assert_eq!(
            header.total_byte_count(),
            MESSAGE_HEADER_WIRE_BYTE_COUNT + 100 + 2 * SLOT_WIRE_BYTE_COUNT
        );
    }

    #[test]
    fn header_round_trips_through_wire_bytes() {
        let header = MessageHeader::new(sender(), 64, 3).unwrap();
        assert_eq!(
            MessageHeader::from_wire_bytes(&header.to_wire_bytes()),
            Ok(header)
        );
    }

    #[test]
    fn short_header_is_truncated() {
        let header = MessageHeader::new(sender(), 0, 0).unwrap();
        let wire_bytes = header.to_wire_bytes();
        assert_eq!(
            MessageHeader::from_wire_bytes(&wire_bytes[..MESSAGE_HEADER_WIRE_BYTE_COUNT - 1]),
            Err(MessageLayoutError::Truncated)
        );
    }

    #[test]
    fn address_slot_round_trips() {
        let slot = Slot::Address(sender());
        assert_eq!(Slot::from_wire_bytes(&slot.to_wire_bytes()), Ok(slot));
    }

    #[test]
    fn scroll_slot_round_trips() {
        let slot = Slot::Scroll(ScrollReference::new(11, 22));
        assert_eq!(Slot::from_wire_bytes(&slot.to_wire_bytes()), Ok(slot));
    }

    #[test]
    fn scroll_slot_with_dirty_padding_is_rejected() {
        let mut wire_bytes = Slot::Scroll(ScrollReference::new(1, 1)).to_wire_bytes();
        wire_bytes[SLOT_WIRE_BYTE_COUNT - 1] = 1;
        assert_eq!(
            Slot::from_wire_bytes(&wire_bytes),
            Err(SlotWireError::NonCanonicalScrollSlot)
        );
    }

    #[test]
    fn empty_input_is_truncated() {
        assert_eq!(Slot::from_wire_bytes(&[]), Err(SlotWireError::Truncated));
    }
}
