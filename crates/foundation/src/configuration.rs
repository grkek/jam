//! How an actor is born and what happens when it runs out of room.

use crate::resource_budget::ResourceBudget;

/// What to do when an actor's inbox or staging queue is full.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// End the actor, so a steward can restart it.
    #[default]
    EndActor,
    /// Discard the message that did not fit and keep the actor running.
    DropNewest,
}

/// Whether an actor's failure travels to its parent. Fixed at birth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lineage {
    /// Failures are not propagated between parent and child.
    #[default]
    Untethered,
    /// The child and its parent are tethered: abnormal endings travel along the tether.
    Tethered,
}

/// What happens to an actor when its parent ends. Fixed at birth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Survival {
    /// The actor ends together with its parent.
    #[default]
    CascadesWithParent,
    /// The actor outlives its parent and is adopted by Genesis.
    Detached,
}

/// Everything chosen when an actor is spawned.
///
/// The resource budget and overflow policy can be adjusted later through a system call.
/// Lineage and survival cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Configuration {
    /// Ceilings on what the actor may consume.
    pub resource_budget: ResourceBudget,
    /// What to do when a ceiling on queued messages is hit.
    pub overflow_policy: OverflowPolicy,
    /// Whether failures travel between this actor and its parent.
    pub lineage: Lineage,
    /// Whether the actor outlives its parent.
    pub survival: Survival,
}

impl Configuration {
    /// Safest choices for everything except the budget, which the system decides.
    #[must_use]
    pub const fn with_budget(resource_budget: ResourceBudget) -> Self {
        Self {
            resource_budget,
            overflow_policy: OverflowPolicy::EndActor,
            lineage: Lineage::Untethered,
            survival: Survival::CascadesWithParent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_configuration_is_the_safest_choice() {
        let configuration = Configuration {
            resource_budget: ResourceBudget {
                memory_budget_in_pages: 0,
                inbox_message_budget: 0,
                inbox_byte_budget: 0,
                staging_queue_message_budget: 0,
            },
            overflow_policy: OverflowPolicy::EndActor,
            lineage: Lineage::Untethered,
            survival: Survival::CascadesWithParent,
        };

        assert_eq!(configuration.overflow_policy, OverflowPolicy::EndActor);
        assert_eq!(configuration.lineage, Lineage::Untethered);
        assert_eq!(configuration.survival, Survival::CascadesWithParent);
        assert_eq!(
            configuration.resource_budget,
            ResourceBudget {
                memory_budget_in_pages: 0,
                inbox_message_budget: 0,
                inbox_byte_budget: 0,
                staging_queue_message_budget: 0,
            }
        );
    }
}
