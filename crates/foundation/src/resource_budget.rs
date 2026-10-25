//! Ceilings the kernel enforces on a single actor.

/// Default memory budget: 256 pages, which is 1 MiB with 4 KiB pages.
pub const DEFAULT_MEMORY_BUDGET_IN_PAGES: u32 = 256;
/// Default number of messages an inbox may hold.
pub const DEFAULT_INBOX_MESSAGE_BUDGET: u32 = 1024;
/// Default number of bytes an inbox may hold: 64 KiB.
pub const DEFAULT_INBOX_BYTE_BUDGET: u32 = 64 * 1024;
/// Default number of messages the kernel stages for an actor before it drains them.
pub const DEFAULT_STAGING_QUEUE_MESSAGE_BUDGET: u32 = 64;

/// How much an actor may consume. Crossing a ceiling triggers the overflow policy or ends the actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceBudget {
    /// Pages of memory the actor may own.
    pub memory_budget_in_pages: u32,
    /// Messages the actor's inbox may hold.
    pub inbox_message_budget: u32,
    /// Bytes the actor's inbox may hold.
    pub inbox_byte_budget: u32,
    /// Messages the kernel may stage for the actor before it drains them.
    pub staging_queue_message_budget: u32,
}

impl ResourceBudget {
    /// Whether every ceiling in this budget is at or below the matching ceiling in `ceiling`.
    ///
    /// The kernel uses this to enforce that an actor may lower its own budget but never raise it.
    #[must_use]
    pub const fn fits_within(self, ceiling: Self) -> bool {
        self.memory_budget_in_pages <= ceiling.memory_budget_in_pages
            && self.inbox_message_budget <= ceiling.inbox_message_budget
            && self.inbox_byte_budget <= ceiling.inbox_byte_budget
            && self.staging_queue_message_budget <= ceiling.staging_queue_message_budget
    }
}

impl Default for ResourceBudget {
    fn default() -> Self {
        Self {
            memory_budget_in_pages: DEFAULT_MEMORY_BUDGET_IN_PAGES,
            inbox_message_budget: DEFAULT_INBOX_MESSAGE_BUDGET,
            inbox_byte_budget: DEFAULT_INBOX_BYTE_BUDGET,
            staging_queue_message_budget: DEFAULT_STAGING_QUEUE_MESSAGE_BUDGET,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_fits_within_itself() {
        let budget = ResourceBudget::default();
        assert!(budget.fits_within(budget));
    }

    #[test]
    fn lowering_one_ceiling_still_fits() {
        let ceiling = ResourceBudget::default();
        let lowered = ResourceBudget {
            memory_budget_in_pages: ceiling.memory_budget_in_pages / 2,
            ..ceiling
        };
        assert!(lowered.fits_within(ceiling));
    }

    #[test]
    fn raising_any_ceiling_does_not_fit() {
        let ceiling = ResourceBudget::default();
        let raised = ResourceBudget {
            inbox_byte_budget: ceiling.inbox_byte_budget + 1,
            ..ceiling
        };
        assert!(!raised.fits_within(ceiling));
    }
}
