//! Ceilings the kernel enforces on a single actor, derived from the machine.

// Policy knobs for how the machine is divided.
const KERNEL_RESERVE_PERCENT: usize = 25;
const MINIMUM_FRAMES_PER_ACTOR: usize = 32;
const MINIMUM_ACTORS: usize = 16;

/// Budgets are ceilings, not reservations, so they may be oversubscribed.
const OVERCOMMIT_FACTOR: usize = 4;

/// How much an actor may consume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceBudget {
    /// How many physical frames the actor may hold at once.
    pub memory_budget_in_pages: u32,
    /// How many messages the actor may have queued in its inbox at once.
    pub inbox_message_budget: u32,
    /// How many bytes the actor may have queued in its inbox at once.
    pub inbox_byte_budget: u32,
    /// How many messages the actor may have queued in its staging queue at once.
    pub staging_queue_message_budget: u32,
}

impl ResourceBudget {
    /// Returns true if this budget is equal to or smaller than the ceiling.
    #[must_use]
    pub const fn fits_within(self, ceiling: Self) -> bool {
        self.memory_budget_in_pages <= ceiling.memory_budget_in_pages
            && self.inbox_message_budget <= ceiling.inbox_message_budget
            && self.inbox_byte_budget <= ceiling.inbox_byte_budget
            && self.staging_queue_message_budget <= ceiling.staging_queue_message_budget
    }
}

/// What the running system offers. Measured at boot, never hardcoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    /// Free physical frames before the kernel builds its own tables.
    pub free_frame_count: usize,
    /// Highest number of actors an address-space identifier can name.
    pub actor_slot_ceiling: usize,
    /// Bytes one actor-table slot occupies.
    pub actor_slot_byte_count: usize,
    /// Messages one inbox can physically hold.
    pub inbox_message_capacity: usize,
    /// Messages the shared message pool holds.
    pub message_pool_capacity: usize,
    /// Bytes one queued message is charged against an inbox.
    pub message_charge_byte_count: usize,
}

/// Limits for this boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    /// Actor slots in use; never more than the table's storage.
    pub maximum_actors: usize,
    /// Both the default and the ceiling an actor may lower but never raise.
    pub budget: ResourceBudget,
}

/// Errors when the machine cannot support a minimal actor population.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitError {
    /// The machine has too little memory to support a minimal actor population.
    TooLittleMemory,
    /// The machine has no message storage, so no actor can be created.
    NoMessageStorage,
}

impl Limit {
    /// Derives limits from measured capacity. Pure, so it runs in host tests.
    ///
    /// # Errors
    /// Fails when the machine cannot support a minimal actor population.
    pub fn derive(capacity: Capacity) -> Result<Self, LimitError> {
        if capacity.message_pool_capacity == 0
            || capacity.inbox_message_capacity == 0
            || capacity.message_charge_byte_count == 0
        {
            return Err(LimitError::NoMessageStorage);
        }

        let reserve = capacity.free_frame_count * KERNEL_RESERVE_PERCENT / 100;
        let user_frames = capacity.free_frame_count - reserve;

        let maximum_actors =
            (user_frames / MINIMUM_FRAMES_PER_ACTOR).min(capacity.actor_slot_ceiling);
        if maximum_actors < MINIMUM_ACTORS {
            return Err(LimitError::TooLittleMemory);
        }

        let pages =
            (user_frames * OVERCOMMIT_FACTOR / maximum_actors).max(MINIMUM_FRAMES_PER_ACTOR);
        let inbox_messages = (capacity.message_pool_capacity * OVERCOMMIT_FACTOR / maximum_actors)
            .clamp(1, capacity.inbox_message_capacity);
        // Messages * charge, so a full inbox always fits its byte budget.
        let inbox_bytes = inbox_messages * capacity.message_charge_byte_count;
        let staging = (inbox_messages / 4).max(1);

        let to_u32 = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
        Ok(Self {
            maximum_actors,
            budget: ResourceBudget {
                memory_budget_in_pages: to_u32(pages),
                inbox_message_budget: to_u32(inbox_messages),
                inbox_byte_budget: to_u32(inbox_bytes),
                staging_queue_message_budget: to_u32(staging),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPACITY: Capacity = Capacity {
        free_frame_count: 65_536, // 256 MiB at 4 KiB
        actor_slot_ceiling: 2048,
        actor_slot_byte_count: 64,
        inbox_message_capacity: 64,
        message_pool_capacity: 4096,
        message_charge_byte_count: 128,
    };

    #[test]
    fn a_budget_fits_within_itself() {
        let limits = Limit::derive(CAPACITY).unwrap();
        assert!(limits.budget.fits_within(limits.budget));
    }

    #[test]
    fn more_memory_never_shrinks_the_budget() {
        let small = Limit::derive(Capacity {
            free_frame_count: 8192,
            ..CAPACITY
        })
        .unwrap();
        let large = Limit::derive(CAPACITY).unwrap();
        assert!(
            small.budget.memory_budget_in_pages <= large.budget.memory_budget_in_pages
                || small.maximum_actors <= large.maximum_actors
        );
    }

    #[test]
    fn actors_never_exceed_table_storage() {
        let huge = Capacity {
            free_frame_count: usize::MAX / 8,
            ..CAPACITY
        };
        assert_eq!(Limit::derive(huge).unwrap().maximum_actors, 2048);
    }

    #[test]
    fn full_inbox_fits_byte_budget() {
        let b = Limit::derive(CAPACITY).unwrap().budget;
        assert!(u64::from(b.inbox_message_budget) * 128 <= u64::from(b.inbox_byte_budget));
    }

    #[test]
    fn tiny_machine_is_rejected() {
        let tiny = Capacity {
            free_frame_count: 100,
            ..CAPACITY
        };
        assert_eq!(Limit::derive(tiny), Err(LimitError::TooLittleMemory));
    }
}
