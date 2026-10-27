//! Everything the kernel knows about one actor.

use foundation::{address::LocalAddress, configuration::Configuration};

use crate::memory::address_space::AddressSpace;

/// Where an actor is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorState {
    /// The slot is unused.
    Free,
    /// The actor is ready and waits in a run queue.
    Runnable,
    /// The actor is running on a core.
    Running,
}

/// One slot of the actor table.
// The parent and configuration are read by the lifecycle code that comes next.
#[allow(dead_code)]
pub struct ControlBlock {
    /// Where the actor is in its life.
    pub state: ActorState,
    /// Changes every time the slot is reused, so a stale address reaches no one.
    pub generation_counter: u32,
    /// Who spawned the actor. `None` for actors the kernel starts itself.
    pub parent_address: Option<LocalAddress>,
    /// What the actor was spawned with. `None` while the slot is free.
    pub configuration: Option<Configuration>,
    /// Address of the frame the actor resumes from while it is not running.
    pub saved_frame_address: usize,
    /// The actor's own lower half. `None` while the slot is free.
    pub address_space: Option<AddressSpace>,
    /// Tags the actor's cached translations, so switching actors needs no TLB flush.
    pub address_space_identifier: u16,
}

impl ControlBlock {
    /// A free slot.
    pub const EMPTY: Self = Self {
        state: ActorState::Free,
        generation_counter: 1,
        parent_address: None,
        configuration: None,
        saved_frame_address: 0,
        address_space: None,
        address_space_identifier: 0,
    };
}
