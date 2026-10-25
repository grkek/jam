//! Why an actor ended.

use crate::address::{Address, NodeIdentifier};

/// The reason an actor ended, as established by the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The actor finished its work and ended on its own.
    Completed,
    /// The actor's parent (or Genesis) terminated it.
    Terminated {
        /// The actor that requested the termination.
        address: Address,
    },
    /// A tethered actor ended abnormally and the failure travelled along the tether.
    Tethered {
        /// The actor whose failure caused this one to fall.
        address: Address,
    },
    /// The actor panicked.
    Panicked,
    /// The actor triggered a hardware fault, such as an invalid memory access.
    Faulted,
    /// The actor used more memory than its resource budget allows.
    MemoryBudgetExhausted,
    /// The actor's inbox grew beyond its resource budget.
    InboxBudgetExhausted,
    /// The actor's parent ended and the actor was not detached.
    ParentDeparted,
    /// The connection to the node hosting the actor was lost.
    ConnectionSevered {
        /// The node that can no longer be reached.
        node_identifier: NodeIdentifier,
    },
    /// The actor was already gone when it was first watched.
    NoSuchActor,
}

impl Cause {
    /// Whether this cause counts as a failure that travels along tethers.
    #[must_use]
    pub const fn is_abnormal(self) -> bool {
        !matches!(self, Self::Completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::LocalAddress;

    #[test]
    fn completing_is_the_only_normal_ending() {
        let someone = Address::Local(LocalAddress::new(1, 1));

        assert!(!Cause::Completed.is_abnormal());
        assert!(Cause::Panicked.is_abnormal());
        assert!(Cause::Faulted.is_abnormal());
        assert!(Cause::Terminated { address: someone }.is_abnormal());
        assert!(Cause::Tethered { address: someone }.is_abnormal());
        assert!(Cause::NoSuchActor.is_abnormal());
    }
}
