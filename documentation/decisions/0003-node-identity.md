# 0003 Node identity

Status: **Accepted**

## Decision

A node is identified by the hash of its public key, so identity cannot be claimed
without the matching private key.

| Part | Choice |
|---|---|
| Key type | Ed25519 |
| Hash | SHA-256, 32 bytes, never truncated |
| Hash input | scheme number (1 byte) followed by the public key |
| Wire form | 1 byte scheme number + 32 byte hash = 33 bytes |
| Scheme `0x01` | Ed25519 + SHA-256 |

## Rules

1. **Hash the scheme with the key.** The same key can never yield valid identifiers
   under two schemes.
2. **Reject unknown schemes.** Nodes refuse identifiers with a scheme number they
   do not support.
3. **Never truncate.** Short forms for display, routing tables, or compact messages
   are local aliases per peer, never a prefix of the identifier.
4. **Opaque outside `foundation`.** Other code compares and copies `NodeIdentifier`;
   it does not slice or interpret the bytes.

## Why

- SHA-256 and Ed25519 are the most scrutinised, widely implemented choices, and SHA-256
  is hardware accelerated on most AArch64 cores.
- 256 bits give about 2^128 collision resistance, which is beyond reach, and about 128
  bits of effective strength against Grover's algorithm.
- The scheme number is the insurance: if an algorithm is ever broken, a new scheme
  coexists with the old while nodes migrate.

## Consequences

- A `RemoteAddress` is 33 bytes of node identifier plus the 8-byte local address.
  `message_layout` must account for that.
