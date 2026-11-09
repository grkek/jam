//! Bounded message protocol between the terminal and its console driver.
//!
//! The kernel supplies the driver's packed local address in the terminal's initial x0.
//! Every subsequent request and response authenticates that address through
//! message metadata. Input is pulled, keeping idle terminals from filling inboxes.

/// Write up to `MAXIMUM_OUTPUT_BYTE_COUNT` bytes; correlation identifies the write.
pub const WRITE_TAG: u64 = 0x434f_0001;
/// Empty response sent once the requested bytes have reached the UART.
pub const WRITTEN_TAG: u64 = 0x434f_0002;
/// Empty request for the next nonempty input chunk.
pub const READ_TAG: u64 = 0x434f_0003;
/// Response containing at most `MAXIMUM_INPUT_BYTE_COUNT` input bytes.
pub const INPUT_TAG: u64 = 0x434f_0004;
/// Maximum bytes written by one output request.
pub const MAXIMUM_OUTPUT_BYTE_COUNT: usize = 192;
/// Maximum bytes returned by one input request.
pub const MAXIMUM_INPUT_BYTE_COUNT: usize = 32;
