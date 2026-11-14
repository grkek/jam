//! Bounded message protocol between the terminal and its console driver.
//!
//! The kernel supplies the driver's packed local address in the terminal's initial x0.
//! Every subsequent request and response authenticates that address through
//! message metadata. Input is pulled, keeping idle terminals from filling inboxes.
//!
//! The terminal also asks the driver for the boot-bound filesystem address. A
//! [`FILE_SYSTEM_BOUND_TAG`] reply with an empty payload means no filesystem is bound.

use crate::{address::LocalAddress, tag::Tag};

pub(crate) const PROTOCOL: u16 = 0x434f;

/// Write up to `MAXIMUM_OUTPUT_BYTE_COUNT` bytes; correlation identifies the write.
pub const WRITE_TAG: Tag = Tag::new(PROTOCOL, 1);
/// Empty response sent once the requested bytes have reached the UART.
pub const WRITTEN_TAG: Tag = Tag::new(PROTOCOL, 2);
/// Empty request for the next nonempty input chunk.
pub const READ_TAG: Tag = Tag::new(PROTOCOL, 3);
/// Response containing at most `MAXIMUM_INPUT_BYTE_COUNT` input bytes.
pub const INPUT_TAG: Tag = Tag::new(PROTOCOL, 4);
/// Empty request for the boot-bound filesystem service address.
pub const FILE_SYSTEM_ADDRESS_TAG: Tag = Tag::new(PROTOCOL, 5);
/// Reply carrying the service's packed address as eight little-endian bytes,
/// or an empty payload when no filesystem is bound.
pub const FILE_SYSTEM_BOUND_TAG: Tag = Tag::new(PROTOCOL, 6);

/// Maximum bytes written by one output request.
pub const MAXIMUM_OUTPUT_BYTE_COUNT: usize = 192;
/// Maximum bytes returned by one input request.
pub const MAXIMUM_INPUT_BYTE_COUNT: usize = 32;

/// Payload size of a bound-filesystem reply.
pub const ADDRESS_BYTE_COUNT: usize = size_of::<u64>();

/// Encodes an address as the payload of a [`FILE_SYSTEM_BOUND_TAG`] reply.
#[must_use]
pub fn encode_address(address: LocalAddress) -> [u8; ADDRESS_BYTE_COUNT] {
    address.to_packed().to_le_bytes()
}

/// Decodes a [`FILE_SYSTEM_BOUND_TAG`] payload. `None` unless exactly [`ADDRESS_BYTE_COUNT`] bytes.
#[must_use]
pub fn decode_address(payload: &[u8]) -> Option<LocalAddress> {
    let bytes: [u8; ADDRESS_BYTE_COUNT] = payload.try_into().ok()?;
    Some(LocalAddress::from_packed(u64::from_le_bytes(bytes)))
}
