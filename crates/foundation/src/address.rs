//! Where an actor can be reached.

use crate::{
    node_identifier::{NODE_IDENTIFIER_WIRE_BYTE_COUNT, NodeIdentifier, UnknownIdentityScheme},
    wire_bytes::{collect_array, read_array},
};

/// Bytes in the serialized form of a local address: slot index plus generation counter.
const LOCAL_ADDRESS_WIRE_BYTE_COUNT: usize = 8;
/// Bytes in the serialized form of any address: a kind byte, a node identifier, a local address.
pub const ADDRESS_WIRE_BYTE_COUNT: usize =
    1 + NODE_IDENTIFIER_WIRE_BYTE_COUNT + LOCAL_ADDRESS_WIRE_BYTE_COUNT;

const LOCAL_ADDRESS_KIND_NUMBER: u8 = 0;
const REMOTE_ADDRESS_KIND_NUMBER: u8 = 1;

/// Address of an actor on the current node.
///
/// The generation counter changes every time a slot is reused, so a stale address
/// can never reach the actor that now occupies the same slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalAddress {
    slot_index: u32,
    generation_counter: u32,
}

impl LocalAddress {
    /// Builds an address from a slot and its generation. The kernel validates it on use.
    #[must_use]
    pub const fn new(slot_index: u32, generation_counter: u32) -> Self {
        Self {
            slot_index,
            generation_counter,
        }
    }

    /// Index of the actor's slot in the actor table.
    #[must_use]
    pub const fn slot_index(self) -> u32 {
        self.slot_index
    }

    /// Generation of the slot this address was issued for.
    #[must_use]
    pub const fn generation_counter(self) -> u32 {
        self.generation_counter
    }

    fn to_wire_bytes(self) -> [u8; LOCAL_ADDRESS_WIRE_BYTE_COUNT] {
        collect_array(
            self.slot_index
                .to_le_bytes()
                .into_iter()
                .chain(self.generation_counter.to_le_bytes()),
        )
    }

    fn from_wire_bytes(wire_bytes: [u8; LOCAL_ADDRESS_WIRE_BYTE_COUNT]) -> Self {
        let [slot_0, slot_1, slot_2, slot_3, generation_0, generation_1, generation_2, generation_3] =
            wire_bytes;
        Self {
            slot_index: u32::from_le_bytes([slot_0, slot_1, slot_2, slot_3]),
            generation_counter: u32::from_le_bytes([
                generation_0,
                generation_1,
                generation_2,
                generation_3,
            ]),
        }
    }
}

/// Address of an actor on another node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RemoteAddress {
    node_identifier: NodeIdentifier,
    local_address: LocalAddress,
}

impl RemoteAddress {
    /// Combines a node with an address that is valid on that node.
    #[must_use]
    pub const fn new(node_identifier: NodeIdentifier, local_address: LocalAddress) -> Self {
        Self {
            node_identifier,
            local_address,
        }
    }

    /// The node that hosts the actor.
    #[must_use]
    pub const fn node_identifier(self) -> NodeIdentifier {
        self.node_identifier
    }

    /// The actor's address as the hosting node knows it.
    #[must_use]
    pub const fn local_address(self) -> LocalAddress {
        self.local_address
    }
}

/// Reference to an actor, with locality made explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Address {
    /// The actor lives on this node.
    Local(LocalAddress),
    /// The actor lives on another node and is reached through distribution.
    Remote(RemoteAddress),
}

/// Why bytes could not be read as an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressWireError {
    /// There were fewer bytes than an address needs.
    Truncated,
    /// The first byte names a kind of address that does not exist.
    UnknownKind {
        /// The number that was received.
        requested_number: u8,
    },
    /// The embedded node identifier uses an identity scheme this node does not support.
    UnknownIdentityScheme(UnknownIdentityScheme),
    /// A local address carried a non-zero node identifier, so it has two encodings.
    NonCanonicalLocalAddress,
}

impl Address {
    /// Whether the actor lives on this node.
    #[must_use]
    pub const fn is_local(self) -> bool {
        match self {
            Self::Local(_) => true,
            Self::Remote(_) => false,
        }
    }

    /// Serializes the address. A local address carries a zeroed node identifier.
    #[must_use]
    pub fn to_wire_bytes(self) -> [u8; ADDRESS_WIRE_BYTE_COUNT] {
        match self {
            Self::Local(local_address) => collect_array(
                core::iter::once(LOCAL_ADDRESS_KIND_NUMBER)
                    .chain([0_u8; NODE_IDENTIFIER_WIRE_BYTE_COUNT])
                    .chain(local_address.to_wire_bytes()),
            ),
            Self::Remote(remote_address) => collect_array(
                core::iter::once(REMOTE_ADDRESS_KIND_NUMBER)
                    .chain(remote_address.node_identifier().to_wire_bytes())
                    .chain(remote_address.local_address().to_wire_bytes()),
            ),
        }
    }

    /// Reads an address from the start of `wire_bytes`. Trailing bytes are ignored.
    ///
    /// # Errors
    ///
    /// Fails when the input is too short, names an unknown address kind or identity scheme,
    /// or encodes a local address non-canonically.
    pub fn from_wire_bytes(wire_bytes: &[u8]) -> Result<Self, AddressWireError> {
        let kind_number = *wire_bytes.first().ok_or(AddressWireError::Truncated)?;
        let node_bytes: [u8; NODE_IDENTIFIER_WIRE_BYTE_COUNT] =
            read_array(wire_bytes, 1).ok_or(AddressWireError::Truncated)?;
        let local_bytes: [u8; LOCAL_ADDRESS_WIRE_BYTE_COUNT] =
            read_array(wire_bytes, 1 + NODE_IDENTIFIER_WIRE_BYTE_COUNT)
                .ok_or(AddressWireError::Truncated)?;
        let local_address = LocalAddress::from_wire_bytes(local_bytes);

        match kind_number {
            LOCAL_ADDRESS_KIND_NUMBER => {
                if node_bytes.iter().any(|node_byte| *node_byte != 0) {
                    Err(AddressWireError::NonCanonicalLocalAddress)
                } else {
                    Ok(Self::Local(local_address))
                }
            }
            REMOTE_ADDRESS_KIND_NUMBER => {
                let node_identifier = NodeIdentifier::from_wire_bytes(node_bytes)
                    .map_err(AddressWireError::UnknownIdentityScheme)?;
                Ok(Self::Remote(RemoteAddress::new(node_identifier, local_address)))
            }
            requested_number => Err(AddressWireError::UnknownKind { requested_number }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_identifier::ED25519_PUBLIC_KEY_BYTE_COUNT;

    fn sample_node_identifier() -> NodeIdentifier {
        NodeIdentifier::from_ed25519_public_key(&[99; ED25519_PUBLIC_KEY_BYTE_COUNT])
    }

    #[test]
    fn local_address_remembers_slot_and_generation() {
        let address = LocalAddress::new(7, 3);
        assert_eq!(address.slot_index(), 7);
        assert_eq!(address.generation_counter(), 3);
    }

    #[test]
    fn same_slot_with_different_generation_is_a_different_address() {
        assert_ne!(LocalAddress::new(7, 3), LocalAddress::new(7, 4));
    }

    #[test]
    fn locality_is_explicit() {
        let local_address = LocalAddress::new(1, 1);
        let node_identifier = sample_node_identifier();
        let remote_address = RemoteAddress::new(node_identifier, local_address);

        assert!(Address::Local(local_address).is_local());
        assert!(!Address::Remote(remote_address).is_local());
        assert_eq!(remote_address.node_identifier(), node_identifier);
        assert_eq!(remote_address.local_address(), local_address);
    }

    #[test]
    fn local_address_round_trips_through_wire_bytes() {
        let address = Address::Local(LocalAddress::new(12, 34));
        assert_eq!(Address::from_wire_bytes(&address.to_wire_bytes()), Ok(address));
    }

    #[test]
    fn remote_address_round_trips_through_wire_bytes() {
        let address = Address::Remote(RemoteAddress::new(
            sample_node_identifier(),
            LocalAddress::new(5, 6),
        ));
        assert_eq!(Address::from_wire_bytes(&address.to_wire_bytes()), Ok(address));
    }

    #[test]
    fn short_input_is_truncated() {
        let wire_bytes = Address::Local(LocalAddress::new(1, 1)).to_wire_bytes();
        assert_eq!(
            Address::from_wire_bytes(&wire_bytes[..ADDRESS_WIRE_BYTE_COUNT - 1]),
            Err(AddressWireError::Truncated)
        );
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let mut wire_bytes = Address::Local(LocalAddress::new(1, 1)).to_wire_bytes();
        wire_bytes[0] = 9;
        assert_eq!(
            Address::from_wire_bytes(&wire_bytes),
            Err(AddressWireError::UnknownKind { requested_number: 9 })
        );
    }

    #[test]
    fn local_address_with_a_node_identifier_is_rejected() {
        let mut wire_bytes = Address::Local(LocalAddress::new(1, 1)).to_wire_bytes();
        wire_bytes[1] = 1;
        assert_eq!(
            Address::from_wire_bytes(&wire_bytes),
            Err(AddressWireError::NonCanonicalLocalAddress)
        );
    }
}
