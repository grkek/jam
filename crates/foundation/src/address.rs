//! Where an actor can be reached.

use crate::node_identifier::NodeIdentifier;

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

impl Address {
    /// Whether the actor lives on this node.
    #[must_use]
    pub const fn is_local(self) -> bool {
        match self {
            Self::Local(_) => true,
            Self::Remote(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_identifier::ED25519_PUBLIC_KEY_BYTE_COUNT;

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
        let node_identifier =
            NodeIdentifier::from_ed25519_public_key(&[99; ED25519_PUBLIC_KEY_BYTE_COUNT]);
        let remote_address = RemoteAddress::new(node_identifier, local_address);

        assert!(Address::Local(local_address).is_local());
        assert!(!Address::Remote(remote_address).is_local());
        assert_eq!(remote_address.node_identifier(), node_identifier);
        assert_eq!(remote_address.local_address(), local_address);
    }
}
