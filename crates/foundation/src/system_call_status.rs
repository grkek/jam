//! What the kernel answers after a system call.
//!
//! The status returns in `x0`. A call that produces a value, such as `CurrentAddress`,
//! returns it in `x1`.

/// Outcome of a system call.
///
/// Sending to, terminating, or watching an actor that has already ended is not an error:
/// those calls are accepted silently so callers cannot probe whether an actor is alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum SystemCallStatus {
    /// The call was carried out.
    Accepted = 0,
    /// The call number does not name a system call.
    UnknownSystemCall = 1,
    /// The caller lacks the authority, for example terminating an actor it did not spawn.
    PermissionDenied = 2,
    /// The call would exceed the caller's resource budget.
    BudgetExhausted = 3,
    /// An address argument is not a valid address, which is different from an actor being dead.
    MalformedAddress = 4,
    /// Another argument is out of range or inconsistent.
    InvalidArgument = 5,
    /// A wait ended because its timeout passed.
    TimedOut = 6,
    /// A scroll reference is stale, forged, or in the wrong state for this call.
    InvalidScrollReference = 7,
}

/// A status number the caller did not expect from the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownSystemCallStatus {
    /// The number that was received.
    pub requested_number: u64,
}

impl SystemCallStatus {
    /// Every status, in numeric order.
    pub const ALL: [Self; 8] = [
        Self::Accepted,
        Self::UnknownSystemCall,
        Self::PermissionDenied,
        Self::BudgetExhausted,
        Self::MalformedAddress,
        Self::InvalidArgument,
        Self::TimedOut,
        Self::InvalidScrollReference,
    ];

    /// The number placed in the status register.
    #[must_use]
    pub const fn number(self) -> u64 {
        self as u64
    }
}

impl TryFrom<u64> for SystemCallStatus {
    type Error = UnknownSystemCallStatus;

    fn try_from(requested_number: u64) -> Result<Self, Self::Error> {
        Self::ALL
            .iter()
            .copied()
            .find(|status| status.number() == requested_number)
            .ok_or(UnknownSystemCallStatus { requested_number })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_is_zero() {
        assert_eq!(SystemCallStatus::Accepted.number(), 0);
    }

    #[test]
    fn every_status_round_trips_through_its_number() {
        for status in SystemCallStatus::ALL {
            assert_eq!(SystemCallStatus::try_from(status.number()), Ok(status));
        }
    }

    #[test]
    fn numbers_are_contiguous_and_match_their_position() {
        for (position, status) in SystemCallStatus::ALL.iter().enumerate() {
            assert_eq!(status.number(), u64::try_from(position).unwrap());
        }
    }

    #[test]
    fn unknown_numbers_are_rejected() {
        assert_eq!(
            SystemCallStatus::try_from(9999),
            Err(UnknownSystemCallStatus { requested_number: 9999 })
        );
    }
}
