//! Cryptographic identity of a node.

use sha2::{Digest, Sha256};

/// Bytes in the public key hash that makes up most of a node identifier.
pub const PUBLIC_KEY_HASH_BYTE_COUNT: usize = 32;
/// Bytes in the serialized form of a node identifier: one scheme byte plus the hash.
pub const NODE_IDENTIFIER_WIRE_BYTE_COUNT: usize = 1 + PUBLIC_KEY_HASH_BYTE_COUNT;
/// Bytes in an Ed25519 public key.
pub const ED25519_PUBLIC_KEY_BYTE_COUNT: usize = 32;

/// Which key type and hash function an identifier was derived with.
///
/// A new scheme number lets identities survive the breaking of an algorithm:
/// old and new identifiers can coexist while nodes migrate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum IdentityScheme {
    /// Ed25519 public key, hashed with SHA-256.
    Ed25519Sha256 = 1,
}

/// An identity scheme number this kernel does not recognise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownIdentityScheme {
    /// The number that was received.
    pub requested_number: u8,
}

impl IdentityScheme {
    /// The number that names this scheme on the wire.
    #[must_use]
    pub const fn number(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for IdentityScheme {
    type Error = UnknownIdentityScheme;

    fn try_from(requested_number: u8) -> Result<Self, Self::Error> {
        match requested_number {
            1 => Ok(Self::Ed25519Sha256),
            _ => Err(UnknownIdentityScheme { requested_number }),
        }
    }
}

/// Identity of a node (a machine) in a distributed system.
///
/// The hash covers the scheme number and the public key together, so the same key
/// can never produce valid identifiers under two schemes. Treat it as opaque: compare
/// and copy it, never slice or truncate it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeIdentifier {
    scheme: IdentityScheme,
    public_key_hash: [u8; PUBLIC_KEY_HASH_BYTE_COUNT],
}

impl NodeIdentifier {
    /// Derives the identifier of the node that owns `public_key`.
    #[must_use]
    pub fn from_ed25519_public_key(public_key: &[u8; ED25519_PUBLIC_KEY_BYTE_COUNT]) -> Self {
        let scheme = IdentityScheme::Ed25519Sha256;

        let mut hasher = Sha256::new();
        hasher.update([scheme.number()]);
        hasher.update(public_key);
        let digest = hasher.finalize();

        let mut public_key_hash = [0_u8; PUBLIC_KEY_HASH_BYTE_COUNT];
        for (destination, source) in public_key_hash.iter_mut().zip(digest.iter()) {
            *destination = *source;
        }

        Self {
            scheme,
            public_key_hash,
        }
    }

    /// The scheme this identifier was derived with.
    #[must_use]
    pub const fn scheme(self) -> IdentityScheme {
        self.scheme
    }

    /// Serializes the identifier: scheme number first, then the hash.
    #[must_use]
    pub fn to_wire_bytes(self) -> [u8; NODE_IDENTIFIER_WIRE_BYTE_COUNT] {
        let mut wire_bytes = [0_u8; NODE_IDENTIFIER_WIRE_BYTE_COUNT];
        let source_bytes = core::iter::once(self.scheme.number()).chain(self.public_key_hash);
        for (destination, source) in wire_bytes.iter_mut().zip(source_bytes) {
            *destination = source;
        }
        wire_bytes
    }

    /// Reads an identifier received from the network.
    ///
    /// # Errors
    ///
    /// Returns `UnknownIdentityScheme` when the first byte names a scheme this node does not support.
    pub fn from_wire_bytes(
        wire_bytes: [u8; NODE_IDENTIFIER_WIRE_BYTE_COUNT],
    ) -> Result<Self, UnknownIdentityScheme> {
        let [scheme_number, public_key_hash @ ..] = wire_bytes;
        let scheme = IdentityScheme::try_from(scheme_number)?;
        Ok(Self {
            scheme,
            public_key_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of the scheme byte `0x01` followed by 32 zero bytes, computed independently.
    const ALL_ZERO_KEY_EXPECTED_HASH: [u8; PUBLIC_KEY_HASH_BYTE_COUNT] = [
        0x1a, 0x7d, 0xfd, 0xea, 0xff, 0xee, 0xda, 0xc4, 0x89, 0x28, 0x7e, 0x85, 0xbe, 0x5e, 0x9c,
        0x04, 0x9a, 0x2f, 0xf6, 0x47, 0x0f, 0x55, 0xcf, 0x30, 0x26, 0x0f, 0x55, 0x39, 0x5a, 0xc1,
        0xb1, 0x59,
    ];

    #[test]
    fn identifier_is_the_hash_of_scheme_number_and_public_key() {
        let node_identifier =
            NodeIdentifier::from_ed25519_public_key(&[0; ED25519_PUBLIC_KEY_BYTE_COUNT]);
        let wire_bytes = node_identifier.to_wire_bytes();

        assert_eq!(
            wire_bytes.first(),
            Some(&IdentityScheme::Ed25519Sha256.number())
        );
        assert_eq!(wire_bytes.get(1..), Some(&ALL_ZERO_KEY_EXPECTED_HASH[..]));
    }

    #[test]
    fn different_keys_give_different_identifiers() {
        let first = NodeIdentifier::from_ed25519_public_key(&[1; ED25519_PUBLIC_KEY_BYTE_COUNT]);
        let second = NodeIdentifier::from_ed25519_public_key(&[2; ED25519_PUBLIC_KEY_BYTE_COUNT]);
        assert_ne!(first, second);
    }

    #[test]
    fn wire_bytes_round_trip() {
        let node_identifier =
            NodeIdentifier::from_ed25519_public_key(&[42; ED25519_PUBLIC_KEY_BYTE_COUNT]);
        let restored = NodeIdentifier::from_wire_bytes(node_identifier.to_wire_bytes());
        assert_eq!(restored, Ok(node_identifier));
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        let mut wire_bytes = [0_u8; NODE_IDENTIFIER_WIRE_BYTE_COUNT];
        wire_bytes[0] = 0;
        assert_eq!(
            NodeIdentifier::from_wire_bytes(wire_bytes),
            Err(UnknownIdentityScheme {
                requested_number: 0
            })
        );
    }
}
