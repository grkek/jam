//! Local copied-message ABI shared by the kernel and actor runtime.
//!
//! TellActor: x0 packed destination, x1 payload pointer, x2 payload length,
//! x3 tag, x4 correlation, x5 reserved (zero). Sender identity comes from the kernel.
//!
//! ReceiveMessage: x0 output pointer, x1 output capacity, x2 selector flags,
//! x3 packed sender filter, x4 tag filter, x5 correlation filter.
//! Accepted returns the record byte count in x1. BufferTooSmall returns the
//! required size without consuming the message. WouldBlock writes no record.
//!
//! AwaitInbox: x0 selector flags, x1 packed sender filter, x2 tag filter,
//! x3 correlation filter, x4 timeout, x5 reserved (zero). Timeout zero polls;
//! u64::MAX waits indefinitely. Other timeouts currently return UnsupportedOperation.
//! The same selector must be used for receive and wait. Wait always rechecks
//! matching queued messages under the actor lock, closing the receive/wait gap.
//!
//! Receive records contain three little-endian u64 words (authenticated packed
//! sender, tag, correlation), followed by payload bytes. Packed local addresses
//! store the slot in the low 32 bits and generation in the high 32 bits.
//!
//! This first ABI supports metadata selection, not arbitrary payload predicates
//! or exported receive cursors. Attachments and remote routing are not provided.

/// Maximum bytes in a copied message payload.
pub const MAXIMUM_PAYLOAD_BYTE_COUNT: usize = 4096;
/// Bytes preceding the payload in a received record.
pub const RECEIVED_HEADER_BYTE_COUNT: usize = 24;
/// Maximum bytes in a complete received record.
pub const MAXIMUM_RECORD_BYTE_COUNT: usize =
    RECEIVED_HEADER_BYTE_COUNT + MAXIMUM_PAYLOAD_BYTE_COUNT;
/// Select only messages from the supplied sender.
pub const MATCH_SENDER: u64 = 1;
/// Select only messages with the supplied tag.
pub const MATCH_TAG: u64 = 2;
/// Select only messages with the supplied correlation identifier.
pub const MATCH_CORRELATION: u64 = 4;
/// Every supported selection flag.
pub const SELECTOR_FLAGS: u64 = MATCH_SENDER | MATCH_TAG | MATCH_CORRELATION;
/// Wait without a deadline.
pub const INDEFINITE_WAIT: u64 = u64::MAX;
