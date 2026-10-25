//! The kernel's full record of how an actor ended.
//!
//! Only the parent, Genesis, and the observatory receive this. Ordinary watchers get a `Farewell`.

use crate::{address::Address, cause::Cause};

/// Number of return addresses captured by a frame-pointer walk.
pub const BACKTRACE_FRAME_MAXIMUM: usize = 8;

/// Hardware state captured when an actor faults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultDetails {
    /// Exception class, taken from `ESR_EL1`.
    pub exception_class: u8,
    /// Value of `ESR_EL1` at the time of the fault.
    pub exception_syndrome_register: u64,
    /// Value of `FAR_EL1`: the address that caused the fault.
    pub fault_address_register: u64,
    /// Where the actor was executing.
    pub program_counter: u64,
    /// The actor's stack pointer.
    pub stack_pointer: u64,
    /// Return addresses from a frame-pointer walk; unused frames are zero.
    pub backtrace_frames: [u64; BACKTRACE_FRAME_MAXIMUM],
}

/// Facts the kernel observed about an actor's life and end. The actor cannot forge any of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Autopsy {
    /// The actor that ended.
    pub address: Address,
    /// Why it ended.
    pub cause: Cause,
    /// Generic timer counter value when the actor ended.
    pub ended_at_ticks: u64,
    /// Ticks between the actor's birth and its end.
    pub lifetime_in_ticks: u64,
    /// The core the actor last ran on.
    pub last_core_identifier: u32,
    /// Reductions the actor executed in total.
    pub reductions_executed: u64,
    /// Most pages the actor owned at any one time.
    pub peak_memory_in_pages: u32,
    /// Most messages the actor's inbox held at any one time.
    pub peak_inbox_message_count: u32,
    /// Messages the actor sent.
    pub messages_sent: u64,
    /// Messages the actor received.
    pub messages_received: u64,
    /// Messages still queued for the actor when it ended, and therefore discarded.
    pub messages_discarded_at_end: u32,
    /// Hardware details, present only when the actor faulted.
    pub fault_details: Option<FaultDetails>,
}
