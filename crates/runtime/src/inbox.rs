//! Safe local copied-message operations backed by JAM's supervisor calls.
//!
//! Selectors match metadata only. Payload predicates, exported cursors, retained
//! references, remote addresses, and finite waits are not implemented yet.

use crate::system_call_wrappers::invoke;
use foundation::{
    address::LocalAddress,
    messaging::{
        INDEFINITE_WAIT, MATCH_CORRELATION, MATCH_SENDER, MATCH_TAG, RECEIVED_HEADER_BYTE_COUNT,
    },
    system_call::SystemCall,
    system_call_status::SystemCallStatus,
};

/// Optional filters applied together when selecting an inbox message.
#[derive(Clone, Copy, Debug, Default)]
pub struct MessageSelector {
    /// Required authenticated sender, or any sender.
    pub sender: Option<LocalAddress>,
    /// Required application tag, or any tag.
    pub tag: Option<u64>,
    /// Required application correlation identifier, or any identifier.
    pub correlation: Option<u64>,
}

impl MessageSelector {
    /// Encodes flags and filter words for both receive and wait.
    fn registers(self) -> [u64; 4] {
        let mut flags = 0;
        if self.sender.is_some() {
            flags |= MATCH_SENDER;
        }
        if self.tag.is_some() {
            flags |= MATCH_TAG;
        }
        if self.correlation.is_some() {
            flags |= MATCH_CORRELATION;
        }
        [
            flags,
            self.sender.map_or(0, pack),
            self.tag.unwrap_or(0),
            self.correlation.unwrap_or(0),
        ]
    }
}

/// A kernel rejection, including the size needed by a short receive buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessagingError {
    /// Recognised status returned by the kernel.
    pub status: SystemCallStatus,
    /// Required record size for `BufferTooSmall`; otherwise zero.
    pub required_byte_count: u64,
}

/// A received message borrowing its payload from the caller's output buffer.
#[derive(Debug)]
pub struct ReceivedMessage<'buffer> {
    /// Kernel-authenticated sender identity.
    pub sender: LocalAddress,
    /// Application tag.
    pub tag: u64,
    /// Application correlation identifier.
    pub correlation: u64,
    /// Copied payload; no references to kernel storage remain.
    pub payload: &'buffer [u8],
}

/// Packs an actor incarnation into the established one-register representation.
fn pack(address: LocalAddress) -> u64 {
    u64::from(address.slot_index()) | (u64::from(address.generation_counter()) << 32)
}

/// Converts register results into a typed status and preserves size feedback.
fn result(status: u64, value: u64) -> Result<u64, MessagingError> {
    let status = SystemCallStatus::try_from(status).unwrap_or(SystemCallStatus::UnknownSystemCall);
    if status == SystemCallStatus::Accepted {
        Ok(value)
    } else {
        Err(MessagingError {
            status,
            required_byte_count: if status == SystemCallStatus::BufferTooSmall {
                value
            } else {
                0
            },
        })
    }
}

/// Sends a copied payload to a local actor. A dead destination discards it.
///
/// # Errors
/// Reports oversized payloads, invalid addresses, or storage exhaustion.
pub fn tell_actor(
    destination: LocalAddress,
    tag: u64,
    correlation: u64,
    payload: &[u8],
) -> Result<(), MessagingError> {
    // SAFETY: The borrowed payload remains readable until the synchronous copy returns.
    let (status, value) = unsafe {
        invoke(
            SystemCall::TellActor.number(),
            [
                pack(destination),
                payload.as_ptr() as u64,
                payload.len() as u64,
                tag,
                correlation,
                0,
            ],
        )
    };
    result(status, value).map(|_| ())
}

/// Reads the caller's address from the scheduler-authenticated kernel context.
///
/// # Errors
/// Returns an error if the kernel cannot identify the caller.
pub fn current_address() -> Result<LocalAddress, MessagingError> {
    // SAFETY: This call has no pointers or input arguments.
    let (status, value) = unsafe { invoke(SystemCall::CurrentAddress.number(), [0; 6]) };
    unpack(result(status, value)?)
}

/// Decodes the canonical local address representation without truncating values.
fn unpack(packed: u64) -> Result<LocalAddress, MessagingError> {
    let error = MessagingError {
        status: SystemCallStatus::MalformedAddress,
        required_byte_count: 0,
    };
    let slot = u32::try_from(packed & u64::from(u32::MAX)).map_err(|_| error)?;
    let generation = u32::try_from(packed >> 32).map_err(|_| error)?;
    Ok(LocalAddress::new(slot, generation))
}

/// Attempts a nonblocking copy into a record buffer, returning its used length.
fn receive_record(selector: MessageSelector, output: &mut [u8]) -> Result<usize, MessagingError> {
    let [flags, sender, tag, correlation] = selector.registers();
    // SAFETY: The exclusive slice remains writable until the kernel finishes copy-out.
    let (status, value) = unsafe {
        invoke(
            SystemCall::ReceiveMessage.number(),
            [
                output.as_mut_ptr() as u64,
                output.len() as u64,
                flags,
                sender,
                tag,
                correlation,
            ],
        )
    };
    usize::try_from(result(status, value)?).map_err(|_| MessagingError {
        status: SystemCallStatus::InvalidArgument,
        required_byte_count: 0,
    })
}

/// Decodes a validated length and fixed little-endian header from a receive buffer.
fn decode(output: &[u8], length: usize) -> Result<ReceivedMessage<'_>, MessagingError> {
    let error = MessagingError {
        status: SystemCallStatus::InvalidArgument,
        required_byte_count: 0,
    };
    let record = output.get(..length).ok_or(error)?;
    let word = |offset: usize| -> Result<u64, MessagingError> {
        let bytes: [u8; 8] = record
            .get(offset..offset + 8)
            .ok_or(error)?
            .try_into()
            .map_err(|_| error)?;
        Ok(u64::from_le_bytes(bytes))
    };
    Ok(ReceivedMessage {
        sender: unpack(word(0)?)?,
        tag: word(8)?,
        correlation: word(16)?,
        payload: record.get(RECEIVED_HEADER_BYTE_COUNT..).ok_or(error)?,
    })
}

/// Copies and consumes the oldest metadata match without waiting.
///
/// # Errors
/// `WouldBlock` means no match. `BufferTooSmall` preserves the queued message and
/// reports the complete required record size, including its 24-byte header.
pub fn receive_message(
    selector: MessageSelector,
    output: &mut [u8],
) -> Result<ReceivedMessage<'_>, MessagingError> {
    let length = receive_record(selector, output)?;
    decode(output, length)
}

/// Waits indefinitely for a metadata match without consuming it.
///
/// # Errors
/// Reports saturated inboxes or invalid kernel state. Use the same selector for
/// the receive attempt and this wait; the kernel rechecks before parking.
pub fn await_inbox(selector: MessageSelector) -> Result<(), MessagingError> {
    let [flags, sender, tag, correlation] = selector.registers();
    // SAFETY: This call carries only scalar filters, not user pointers.
    let (status, value) = unsafe {
        invoke(
            SystemCall::AwaitInbox.number(),
            [flags, sender, tag, correlation, INDEFINITE_WAIT, 0],
        )
    };
    result(status, value).map(|_| ())
}

/// Receives a metadata match, waiting indefinitely when none is queued.
///
/// # Errors
/// Propagates copy, capacity, and admission errors. This does not evaluate
/// arbitrary predicates over message payloads.
pub fn await_matching(
    selector: MessageSelector,
    output: &mut [u8],
) -> Result<ReceivedMessage<'_>, MessagingError> {
    loop {
        match receive_record(selector, output) {
            Ok(length) => return decode(output, length),
            Err(error) if error.status == SystemCallStatus::WouldBlock => await_inbox(selector)?,
            Err(error) => return Err(error),
        }
    }
}
