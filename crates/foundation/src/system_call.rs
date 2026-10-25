//! The contract between actors and the kernel, expressed in registers.
//!
//! An actor places the call number in register `x8` and arguments in `x0` to `x5`,
//! then executes `svc`. The result returns in `x0`.

/// Register that carries the system call number.
pub const SYSTEM_CALL_NUMBER_REGISTER: u8 = 8;
/// How many argument registers (`x0` to `x5`) a call may use.
pub const ARGUMENT_REGISTER_COUNT: usize = 6;
/// Register that carries the result.
pub const RESULT_REGISTER: u8 = 0;

/// Every operation an actor can ask the kernel to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum SystemCall {
    /// Create an actor from a `Configuration`.
    SpawnActor = 0,
    /// Terminate a child. Allowed only for the parent and Genesis.
    TerminateActor = 1,
    /// Ask to receive a `Farewell` when an actor ends. Open to everyone.
    WatchActor = 2,
    /// Stop watching an actor.
    UnwatchActor = 3,
    /// Receive tethered failures as farewells instead of falling with them.
    BecomeGuardian = 4,
    /// Deliver a message to an actor, asynchronously.
    TellActor = 5,
    /// Sleep until a message arrives beyond the inbox cursor, or a timeout passes.
    AwaitInbox = 6,
    /// End the calling actor.
    FinishActor = 7,
    /// Learn the calling actor's own address.
    CurrentAddress = 8,
    /// Change the calling actor's, or a child's, budget and overflow policy.
    AdjustConfiguration = 9,
    /// Obtain pages of memory for the actor's own heap.
    AllocatePages = 10,
    /// Return pages of memory.
    ReleasePages = 11,
    /// Read a snapshot of the actor table.
    ReadCensus = 12,
    /// Read a snapshot of one core's state.
    ReadCoreCensus = 13,
    /// Drain recorded events from the chronicle.
    DrainChronicle = 14,
    /// Turn chronicle event classes on or off.
    ConfigureChronicle = 15,
}

/// A system call number the kernel does not recognise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownSystemCall {
    /// The number that was requested.
    pub requested_number: u64,
}

impl SystemCall {
    /// Every system call, in numeric order.
    pub const ALL: [Self; 16] = [
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
        assert_eq!(
            SystemCall::try_from(9999),
            Err(UnknownSystemCall { requested_number: 9999 })
        );
    }
}
