//! Ceilings the kernel enforces on a single actor, derived from the machine.
//!
//! The kernel measures what the machine offers at boot ([`Capacity`]) and turns it into a
//! population size and a per-actor [`ResourceBudget`] ([`Limit`]). Every actor starts with that
//! budget and may lower it, but never raise it.

use crate::physical_memory::PHYSICAL_FRAME_BYTE_COUNT;

/// Maximum number of simultaneously allocated kernel messages (the shared message pool).
pub const MAXIMUM_POOL_MESSAGE_COUNT: usize = 128;

/// Maximum payload bytes in one kernel-owned message.
pub const MAXIMUM_MESSAGE_PAYLOAD_BYTE_COUNT: usize = crate::messaging::MAXIMUM_PAYLOAD_BYTE_COUNT;

/// Maximum number of messages one actor's inbox may hold at once, including a message currently
/// leased for copy-out. Bounds per-actor descriptor storage and the O(n) scans in selective
/// receive.
pub const MAXIMUM_INBOX_MESSAGE_COUNT: usize = 64;

/// Maximum resident bytes one actor's inbox may be charged, in the same units as
/// `MessageCharge` (message-pool slot plus envelope per message).
///
/// A second quota alongside [`MAXIMUM_INBOX_MESSAGE_COUNT`]: whichever limit is reached first
/// rejects further sends with `SendError::BudgetExceeded` or `Full`.
pub const MAXIMUM_INBOX_BYTE_COUNT: usize = 256 * 1024;

/// The inbox byte budget is stored as a `u32` in [`ResourceBudget`].
const _: () = assert!(MAXIMUM_INBOX_BYTE_COUNT <= u32::MAX as usize);

/// One staging-queue slot is granted per this many inbox messages.
const INBOX_MESSAGE_COUNT_PER_STAGING_SLOT: usize = 4;

/// Percentage of free physical frames held back for the kernel itself.
const KERNEL_RESERVED_MEMORY_PERCENT: usize = 25;

/// Fewest physical frames any single actor is guaranteed to be able to hold.
const MINIMUM_ACTOR_MEMORY_FRAME_COUNT: usize = 32;

/// Fewest actors the machine must support for the kernel to boot.
const MINIMUM_ACTOR_COUNT: usize = 16;

/// Memory ceilings are budgets, not reservations, so the sum of all ceilings may exceed physical
/// memory by this factor.
const MEMORY_OVERCOMMIT_FACTOR: usize = 4;

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

impl Capacity {
    /// Builds a capacity from the boot-time measurements plus the kernel's own compile-time
    /// message limits ([`MAXIMUM_POOL_MESSAGE_COUNT`] and [`MAXIMUM_INBOX_MESSAGE_COUNT`]).
    #[must_use]
    pub const fn with_kernel_message_limits(
        free_frame_count: usize,
        actor_slot_ceiling: usize,
        actor_slot_byte_count: usize,
        message_charge_byte_count: usize,
    ) -> Self {
        Self {
            free_frame_count,
            actor_slot_ceiling,
            actor_slot_byte_count,
            inbox_message_capacity: MAXIMUM_INBOX_MESSAGE_COUNT,
            message_pool_capacity: MAXIMUM_POOL_MESSAGE_COUNT,
            message_charge_byte_count,
        }
    }
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

/// How many actors fit and how many frames remain for them to share.
#[derive(Clone, Copy, Debug)]
struct ActorPopulation {
    /// Actor slots in use.
    maximum_actors: usize,
    /// Frames left for actor memory once the kernel reserve and the actor table itself have been
    /// paid for.
    user_frame_count: u128,
}

/// Widens to `u128` so intermediate products cannot overflow.
const fn widen_to_u128(value: usize) -> u128 {
    value as u128
}

/// Narrows to `u32`, saturating rather than truncating.
fn saturate_to_u32(value: u128) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Returns the number of whole frames needed to store `byte_count` bytes.
fn frame_count_for_byte_count(byte_count: u128) -> u128 {
    // `PHYSICAL_FRAME_BYTE_COUNT` is a `u64`, so it converts losslessly.
    byte_count.div_ceil(u128::from(PHYSICAL_FRAME_BYTE_COUNT))
}

/// Works out how many actors the machine can hold.
///
/// Memory is the kernel reserve first, then a per-actor cost: the guaranteed minimum frames plus
/// the frames the actor's table slot occupies. The result is clamped to the number of slots an
/// address-space identifier can name.
fn derive_actor_population(capacity: Capacity) -> Result<ActorPopulation, LimitError> {
    let free_frame_count = widen_to_u128(capacity.free_frame_count);
    let kernel_reserved_frame_count =
        free_frame_count * widen_to_u128(KERNEL_RESERVED_MEMORY_PERCENT) / 100;
    let available_frame_count = free_frame_count - kernel_reserved_frame_count;

    // Actor slots consume physical memory even before actors run.
    let table_frame_count_per_actor =
        frame_count_for_byte_count(widen_to_u128(capacity.actor_slot_byte_count));
    let minimum_frame_cost_per_actor =
        widen_to_u128(MINIMUM_ACTOR_MEMORY_FRAME_COUNT) + table_frame_count_per_actor;

    let maximum_actors = (available_frame_count / minimum_frame_cost_per_actor)
        .min(widen_to_u128(capacity.actor_slot_ceiling));

    if maximum_actors < widen_to_u128(MINIMUM_ACTOR_COUNT) {
        return Err(LimitError::TooLittleMemory);
    }

    // The count was clamped to `actor_slot_ceiling`, which is a `usize`, so this cannot fail.
    let maximum_actors = usize::try_from(maximum_actors).unwrap_or(usize::MAX);

    let actor_table_frame_count = frame_count_for_byte_count(
        widen_to_u128(maximum_actors) * widen_to_u128(capacity.actor_slot_byte_count),
    );

    Ok(ActorPopulation {
        maximum_actors,
        user_frame_count: available_frame_count.saturating_sub(actor_table_frame_count),
    })
}

/// Splits user memory across actors, deliberately oversubscribed.
///
/// Ceilings may overlap (see [`MEMORY_OVERCOMMIT_FACTOR`]), but physical memory is never oversold:
/// no single actor's ceiling exceeds all user frames, and none falls below the guaranteed minimum.
fn derive_memory_budget(population: ActorPopulation) -> u32 {
    let overcommitted_share = population.user_frame_count * widen_to_u128(MEMORY_OVERCOMMIT_FACTOR)
        / widen_to_u128(population.maximum_actors);

    let memory_budget_in_pages = overcommitted_share
        .max(widen_to_u128(MINIMUM_ACTOR_MEMORY_FRAME_COUNT))
        .min(population.user_frame_count);

    saturate_to_u32(memory_budget_in_pages)
}

/// Derives the per-actor inbox message ceiling.
///
/// This is a per-actor ceiling, and the shared message pool is a separate global constraint.
/// Dividing pool capacity by actor slots would wrongly force thousands of actors down to one
/// message each, so the pool only caps the ceiling from above.
fn derive_inbox_message_budget(capacity: Capacity) -> Result<u32, LimitError> {
    if capacity.message_pool_capacity == 0
        || capacity.inbox_message_capacity == 0
        || capacity.message_charge_byte_count == 0
    {
        return Err(LimitError::NoMessageStorage);
    }

    // How many messages fit under the inbox byte ceiling at this charge.
    let message_count_under_byte_ceiling =
        MAXIMUM_INBOX_BYTE_COUNT / capacity.message_charge_byte_count;

    let inbox_message_budget = MAXIMUM_INBOX_MESSAGE_COUNT
        .min(capacity.inbox_message_capacity)
        .min(capacity.message_pool_capacity)
        .min(message_count_under_byte_ceiling);

    // A single message costing more than the whole byte ceiling can never be queued, so no actor
    // could receive anything.
    if inbox_message_budget == 0 {
        return Err(LimitError::NoMessageStorage);
    }

    Ok(saturate_to_u32(widen_to_u128(inbox_message_budget)))
}

/// Returns the bytes a completely full inbox is charged.
///
/// By construction this is at most [`MAXIMUM_INBOX_BYTE_COUNT`], so the message and byte quotas
/// agree.
fn derive_inbox_byte_budget(inbox_message_budget: u32, capacity: Capacity) -> u32 {
    let inbox_byte_budget =
        u128::from(inbox_message_budget) * widen_to_u128(capacity.message_charge_byte_count);

    saturate_to_u32(inbox_byte_budget)
}

/// Gives the staging queue a fraction of the inbox allowance, never less than one message.
fn derive_staging_queue_message_budget(inbox_message_budget: u32) -> u32 {
    let staging_queue_message_budget =
        u128::from(inbox_message_budget) / widen_to_u128(INBOX_MESSAGE_COUNT_PER_STAGING_SLOT);

    saturate_to_u32(staging_queue_message_budget.max(1))
}

impl Limit {
    /// Derives hardware-aware resource ceilings from available capacity.
    ///
    /// # Errors
    /// Returns an error when the machine cannot support the minimum
    /// actor population or a valid message budget.
    pub fn derive(capacity: Capacity) -> Result<Self, LimitError> {
        // Message storage is validated first so a machine with neither messages nor memory
        // reports the more fundamental problem.
        let inbox_message_budget = derive_inbox_message_budget(capacity)?;
        let population = derive_actor_population(capacity)?;

        Ok(Self {
            maximum_actors: population.maximum_actors,
            budget: ResourceBudget {
                memory_budget_in_pages: derive_memory_budget(population),
                inbox_message_budget,
                inbox_byte_budget: derive_inbox_byte_budget(inbox_message_budget, capacity),
                staging_queue_message_budget: derive_staging_queue_message_budget(
                    inbox_message_budget,
                ),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    //! Host-side checks for budget derivation, quota agreement, and overflow safety.

    use super::*;

    /// A 256 MiB machine with plenty of message storage.
    const CAPACITY: Capacity = Capacity {
        free_frame_count: 65_536, // 256 MiB at 4 KiB
        actor_slot_ceiling: 2048,
        actor_slot_byte_count: 64,
        inbox_message_capacity: 64,
        message_pool_capacity: 4096,
        message_charge_byte_count: 128,
    };

    /// A budget fits within itself.
    #[test]
    fn a_budget_fits_within_itself() {
        let limits = Limit::derive(CAPACITY).unwrap();
        assert!(limits.budget.fits_within(limits.budget));
    }

    /// Adding memory never reduces how many actors fit.
    #[test]
    fn more_memory_never_shrinks_the_population() {
        let small_limits = Limit::derive(Capacity {
            free_frame_count: 8192,
            ..CAPACITY
        })
        .unwrap();
        let large_limits = Limit::derive(CAPACITY).unwrap();

        assert!(small_limits.maximum_actors <= large_limits.maximum_actors);
    }

    /// The population never exceeds the actor table's storage.
    #[test]
    fn actors_never_exceed_table_storage() {
        let huge_capacity = Capacity {
            free_frame_count: usize::MAX / 8,
            ..CAPACITY
        };

        assert_eq!(Limit::derive(huge_capacity).unwrap().maximum_actors, 2048);
    }

    /// The frame arithmetic is widened so the largest possible frame count cannot overflow.
    #[test]
    fn enormous_memory_capacity_does_not_overflow() {
        let capacity = Capacity {
            free_frame_count: usize::MAX,
            ..CAPACITY
        };

        assert_eq!(Limit::derive(capacity).unwrap().maximum_actors, 2048);
    }

    /// A machine that cannot hold the minimum population is refused.
    #[test]
    fn tiny_machine_is_rejected() {
        let tiny_capacity = Capacity {
            free_frame_count: 100,
            ..CAPACITY
        };

        assert_eq!(
            Limit::derive(tiny_capacity),
            Err(LimitError::TooLittleMemory)
        );
    }

    /// Larger actor-table slots leave room for fewer actors.
    #[test]
    fn actor_table_storage_reduces_population() {
        let compact_limits = Limit::derive(Capacity {
            actor_slot_byte_count: 64,
            ..CAPACITY
        })
        .unwrap();
        let large_limits = Limit::derive(Capacity {
            actor_slot_byte_count: 64 * 1024,
            ..CAPACITY
        })
        .unwrap();

        assert!(large_limits.maximum_actors < compact_limits.maximum_actors);
    }

    /// Every actor's memory ceiling is at least the guaranteed minimum and at most all user memory.
    #[test]
    fn memory_budget_stays_between_minimum_and_user_memory() {
        let memory_budget_in_pages = Limit::derive(CAPACITY)
            .unwrap()
            .budget
            .memory_budget_in_pages;

        assert!(
            u128::from(memory_budget_in_pages) >= widen_to_u128(MINIMUM_ACTOR_MEMORY_FRAME_COUNT)
        );
        assert!(u128::from(memory_budget_in_pages) <= widen_to_u128(CAPACITY.free_frame_count));
    }

    /// Ceilings are oversubscribed: together they promise more memory than the machine has.
    #[test]
    fn memory_ceilings_are_overcommitted() {
        let limits = Limit::derive(CAPACITY).unwrap();
        let total_ceiling_frame_count =
            u128::from(limits.budget.memory_budget_in_pages) * widen_to_u128(limits.maximum_actors);

        assert!(total_ceiling_frame_count > widen_to_u128(CAPACITY.free_frame_count));
    }

    /// A completely full inbox fits within its byte budget.
    #[test]
    fn full_inbox_fits_byte_budget() {
        let budget = Limit::derive(CAPACITY).unwrap().budget;

        assert!(
            u64::from(budget.inbox_message_budget) * 128 <= u64::from(budget.inbox_byte_budget)
        );
    }

    /// The derived inbox quotas never exceed the kernel-wide ceilings.
    #[test]
    fn inbox_budget_never_exceeds_kernel_ceilings() {
        let budget = Limit::derive(CAPACITY).unwrap().budget;

        assert!(
            widen_to_u128(MAXIMUM_INBOX_MESSAGE_COUNT) >= u128::from(budget.inbox_message_budget)
        );
        assert!(widen_to_u128(MAXIMUM_INBOX_BYTE_COUNT) >= u128::from(budget.inbox_byte_budget));
    }

    /// Large message charges lower the message count so the byte ceiling is still respected.
    #[test]
    fn inbox_byte_ceiling_limits_message_count_for_large_charges() {
        // 256 KiB / 8 KiB = 32 messages, fewer than the 64-message ceiling.
        let capacity = Capacity {
            message_charge_byte_count: 8 * 1024,
            ..CAPACITY
        };
        let budget = Limit::derive(capacity).unwrap().budget;

        assert_eq!(budget.inbox_message_budget, 32);
        assert_eq!(
            u128::from(budget.inbox_byte_budget),
            widen_to_u128(MAXIMUM_INBOX_BYTE_COUNT)
        );
    }

    /// An inbox that physically holds fewer messages lowers the budget to match.
    #[test]
    fn inbox_budget_is_capped_by_physical_inbox_capacity() {
        let capacity = Capacity {
            inbox_message_capacity: 8,
            ..CAPACITY
        };

        assert_eq!(
            Limit::derive(capacity).unwrap().budget.inbox_message_budget,
            8
        );
    }

    /// Unbounded pool and inbox capacities cannot overflow the byte budget.
    #[test]
    fn inbox_budget_does_not_overflow() {
        let capacity = Capacity {
            message_pool_capacity: usize::MAX,
            inbox_message_capacity: usize::MAX,
            message_charge_byte_count: 1024,
            ..CAPACITY
        };
        let budget = Limit::derive(capacity).unwrap().budget;

        assert!(
            u64::from(budget.inbox_message_budget) * 1024 <= u64::from(budget.inbox_byte_budget)
        );
    }

    /// The staging queue gets a quarter of the inbox, and never less than one message.
    #[test]
    fn staging_queue_budget_is_a_fraction_of_the_inbox_and_at_least_one() {
        let budget = Limit::derive(CAPACITY).unwrap().budget;
        assert_eq!(
            usize::try_from(budget.staging_queue_message_budget).unwrap(),
            MAXIMUM_INBOX_MESSAGE_COUNT / INBOX_MESSAGE_COUNT_PER_STAGING_SLOT
        );

        let single_message_capacity = Capacity {
            inbox_message_capacity: 1,
            ..CAPACITY
        };
        let single_message_budget = Limit::derive(single_message_capacity).unwrap().budget;
        assert_eq!(single_message_budget.staging_queue_message_budget, 1);
    }

    /// A message charge that does not fit a `u32` leaves no usable message storage.
    #[test]
    fn oversized_message_charge_is_rejected() {
        let capacity = Capacity {
            message_charge_byte_count: u32::MAX as usize + 1,
            ..CAPACITY
        };

        assert_eq!(Limit::derive(capacity), Err(LimitError::NoMessageStorage));
    }

    /// A message charge larger than the whole inbox byte ceiling can never be queued.
    #[test]
    fn charge_larger_than_inbox_byte_ceiling_is_rejected() {
        let capacity = Capacity {
            message_charge_byte_count: MAXIMUM_INBOX_BYTE_COUNT + 1,
            ..CAPACITY
        };

        assert_eq!(Limit::derive(capacity), Err(LimitError::NoMessageStorage));
    }

    /// Zero-sized message storage of any kind is refused.
    #[test]
    fn missing_message_storage_is_rejected() {
        for capacity in [
            Capacity {
                message_pool_capacity: 0,
                ..CAPACITY
            },
            Capacity {
                inbox_message_capacity: 0,
                ..CAPACITY
            },
            Capacity {
                message_charge_byte_count: 0,
                ..CAPACITY
            },
        ] {
            assert_eq!(Limit::derive(capacity), Err(LimitError::NoMessageStorage));
        }
    }

    /// The boot-time constructor fills in the kernel's own message limits.
    #[test]
    fn kernel_message_limits_constructor_uses_the_kernel_constants() {
        let capacity = Capacity::with_kernel_message_limits(65_536, 2048, 64, 128);

        assert_eq!(capacity.message_pool_capacity, MAXIMUM_POOL_MESSAGE_COUNT);
        assert_eq!(capacity.inbox_message_capacity, MAXIMUM_INBOX_MESSAGE_COUNT);
        assert!(Limit::derive(capacity).is_ok());
    }
}
