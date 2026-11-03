//! The contract between actors and the kernel, expressed in registers.
//!
//! An actor places the call number in register `x8` and arguments in `x0` to `x5`,
//! then executes `svc #0`. The kernel answers with a `SystemCallStatus` in `x0`
//! and, for calls that produce one, a value in `x1`.
//!
//! Existing call numbers are stable. New operations are appended rather than
//! inserted, and retired numbers must not be reassigned.
//!
//! This module assigns operation numbers; it does not imply that every
//! operation has a kernel implementation.
//!
//! Stewards implement restart policy in userspace using ordinary actor
//! operations. Tethers are established through spawn configuration only.
//!
//! The runtime exposes selective receive as `await_matching`. Its kernel
//! operations are `ReceiveMessage` for nonblocking selection and consumption,
//! and `AwaitInbox` for waiting. Their implementation must coordinate cursor
//! validation, wait registration, and message arrival to prevent lost wakeups.

/// Register that carries the system call number.
pub const SYSTEM_CALL_NUMBER_REGISTER: u8 = 8;

/// How many argument registers (`x0` to `x5`) a call may use.
pub const ARGUMENT_REGISTER_COUNT: usize = 6;

/// Register that carries the `SystemCallStatus`.
pub const STATUS_REGISTER: u8 = 0;

/// Register that carries the value a call produces, if any.
pub const VALUE_REGISTER: u8 = 1;

/// Every operation defined by the actor-to-kernel interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum SystemCall {
    /// Create an actor from a `Configuration`.
    ///
    /// Spawn configuration determines whether the child is tethered or
    /// detached. A tether cannot be established after birth.
    SpawnActor = 0,

    /// Terminate an actor. Allowed only for its parent and Genesis.
    TerminateActor = 1,

    /// Ask to receive a `Farewell` when an actor ends.
    ///
    /// Watching is open to every actor and grants no termination authority.
    WatchActor = 2,

    /// Stop watching an actor.
    UnwatchActor = 3,

    /// Receive tethered failures as farewells instead of falling with them.
    BecomeGuardian = 4,

    /// Deliver a message to an actor asynchronously.
    ///
    /// Delivery to a dead actor is silently discarded.
    TellActor = 5,

    /// Wait for inbox progress beyond a receive cursor, or for a timeout.
    ///
    /// Waiting does not consume a message. The kernel must check for relevant
    /// progress while registering the wait so a concurrent send cannot be lost.
    AwaitInbox = 6,

    /// End the calling actor.
    ///
    /// Successful termination does not return to the caller.
    FinishActor = 7,

    /// Learn the calling actor's own address.
    CurrentAddress = 8,

    /// Change the calling actor's, or a child's, budget and overflow policy.
    AdjustConfiguration = 9,

    /// Obtain pages of memory for the actor's own heap.
    AllocatePages = 10,

    /// Return previously allocated heap pages owned by the calling actor.
    ReleasePages = 11,

    /// Read a snapshot of the actor table.
    ReadCensus = 12,

    /// Read a snapshot of one core's state.
    ReadCoreCensus = 13,

    /// Drain recorded events from the chronicle.
    DrainChronicle = 14,

    /// Turn chronicle event classes on or off.
    ConfigureChronicle = 15,

    /// Create a draft Scroll and receive a writable view of it.
    CreateScroll = 16,

    /// Seal a draft Scroll.
    ///
    /// After sealing succeeds, nobody may write to it, including its creator.
    SealScroll = 17,

    /// Map a sealed Scroll read-only into the calling actor.
    MapScroll = 18,

    /// Remove a Scroll mapping while retaining the caller's Scroll reference.
    ///
    /// Unmapping does not release the Scroll or affect another actor's mapping.
    UnmapScroll = 19,

    /// Drop the calling actor's reference to a Scroll.
    ReleaseScroll = 20,

    /// Select and copy a matching message without blocking.
    ///
    /// Selection uses kernel-supported metadata and an optional receive
    /// cursor, not an actor-supplied executable predicate.
    ///
    /// A successful receive consumes the selected message. An insufficient
    /// destination buffer or failed copy must not consume it while the inbox
    /// remains open. Messages that do not match remain queued.
    ReceiveMessage = 21,

    /// Voluntarily relinquish the calling actor's current execution turn.
    ///
    /// The actor remains runnable and may resume immediately if no other
    /// actor is eligible to run.
    YieldExecution = 22,

    /// Read the current node's monotonic clock in nanoseconds.
    ///
    /// The clock has a node-local origin and is not a wall clock. Values from
    /// different nodes must not be treated as synchronized timestamps.
    ReadMonotonicTime = 23,

    /// Map an authorised device-memory region into the calling actor.
    ///
    /// The kernel validates device authority, bounds, permissions, and memory
    /// attributes. This call does not grant arbitrary physical-memory access.
    MapDeviceMemory = 24,

    /// Remove a device-memory mapping owned by the calling actor.
    ///
    /// Unmapping does not free the underlying device memory.
    UnmapDeviceMemory = 25,

    /// Bind an authorised interrupt source to the calling driver actor.
    ///
    /// The kernel retains control of hardware acknowledgement and masking.
    BindInterrupt = 26,

    /// Release an interrupt binding owned by the calling actor.
    ///
    /// The kernel must prevent further notifications from using the retired
    /// binding.
    UnbindInterrupt = 27,

    /// Report completion of a delivered interrupt notification.
    ///
    /// The kernel validates the binding and notification before performing
    /// any required controller operation or re-enabling delivery. Stale or
    /// duplicate completions must not affect a newer interrupt notification.
    CompleteInterrupt = 28,

    /// Read the full kernel record of an actor's termination.
    ///
    /// Access is restricted to the actor's parent, Genesis, and an authorised
    /// Observatory. Records are subject to the kernel's retention limits.
    ReadAutopsy = 29,

    /// Read retained records from the always-on flight recorder.
    ///
    /// Requires observation authority. Reading does not consume records.
    /// The kernel reports when requested records have been overwritten.
    ReadBlackbox = 30,

    /// Allocate a buffer for direct memory access by an authorised device.
    ///
    /// The kernel validates device authority, size, alignment, and addressing
    /// constraints. Device-visible addresses need not equal physical addresses.
    DirectMemoryAccessAllocate = 31,

    /// Release a direct memory access allocation owned by the calling actor.
    ///
    /// Device access must be quiesced before the allocation can be reclaimed.
    /// Memory must not be reused while the device can still access it.
    DirectMemoryAccessRelease = 32,

    /// Synchronize a direct memory access buffer for a CPU/device handoff.
    ///
    /// The request identifies the buffer range, transfer direction, and new
    /// owner. The kernel performs the required cache maintenance and ordering.
    /// Synchronization does not itself wait for device transfer completion.
    DirectMemoryAccessSynchronize = 33,
}

/// A system call number the kernel does not recognise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownSystemCall {
    /// The number that was requested.
    pub requested_number: u64,
}

impl SystemCall {
    /// Every system call, in numeric order.
    pub const ALL: [Self; 34] = [
        Self::SpawnActor,
        Self::TerminateActor,
        Self::WatchActor,
        Self::UnwatchActor,
        Self::BecomeGuardian,
        Self::TellActor,
        Self::AwaitInbox,
        Self::FinishActor,
        Self::CurrentAddress,
        Self::AdjustConfiguration,
        Self::AllocatePages,
        Self::ReleasePages,
        Self::ReadCensus,
        Self::ReadCoreCensus,
        Self::DrainChronicle,
        Self::ConfigureChronicle,
        Self::CreateScroll,
        Self::SealScroll,
        Self::MapScroll,
        Self::UnmapScroll,
        Self::ReleaseScroll,
        Self::ReceiveMessage,
        Self::YieldExecution,
        Self::ReadMonotonicTime,
        Self::MapDeviceMemory,
        Self::UnmapDeviceMemory,
        Self::BindInterrupt,
        Self::UnbindInterrupt,
        Self::CompleteInterrupt,
        Self::ReadAutopsy,
        Self::ReadBlackbox,
        Self::DirectMemoryAccessAllocate,
        Self::DirectMemoryAccessRelease,
        Self::DirectMemoryAccessSynchronize,
    ];

    /// The number placed in the system call register.
    #[must_use]
    pub const fn number(self) -> u64 {
        self as u64
    }
}

impl TryFrom<u64> for SystemCall {
    type Error = UnknownSystemCall;

    fn try_from(requested_number: u64) -> Result<Self, Self::Error> {
        Self::ALL
            .iter()
            .copied()
            .find(|system_call| system_call.number() == requested_number)
            .ok_or(UnknownSystemCall { requested_number })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_contiguous_and_match_their_position() {
        for (position, system_call) in SystemCall::ALL.iter().enumerate() {
            assert_eq!(system_call.number(), u64::try_from(position).unwrap());
        }
    }

    #[test]
    fn every_system_call_round_trips_through_its_number() {
        for system_call in SystemCall::ALL {
            assert_eq!(SystemCall::try_from(system_call.number()), Ok(system_call));
        }
    }

    #[test]
    fn unknown_numbers_are_rejected() {
        let first_unknown_number = u64::try_from(SystemCall::ALL.len()).unwrap();

        for requested_number in [first_unknown_number, 9999, u64::MAX] {
            assert_eq!(
                SystemCall::try_from(requested_number),
                Err(UnknownSystemCall { requested_number })
            );
        }
    }
}
