//! Data that belongs to exactly one core.
//!
//! Each core records its own index in `TPIDR_EL1` as it starts, so finding "my" data is
//! a register read and an array lookup, with no lock and no search.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use foundation::run_queue::RunQueue;

use crate::{
    actors::table::ACTOR_MAXIMUM,
    architecture::aarch64::processor::{read_current_core_index, write_current_core_index},
    synchronization::spin_lock::SpinLock,
};

/// Most cores this kernel supports. Matches `CORE_COUNT` in the linker script.
pub const CORE_MAXIMUM: usize = 8;
/// Stands in for a slot index when a core runs no actor.
pub const NO_ACTOR: usize = usize::MAX;

/// One core's data, aligned to a cache line so cores never contend for the same line.
#[repr(align(64))]
pub struct CoreLocal {
    core_index: usize,
    is_online: AtomicBool,
    run_queue: SpinLock<RunQueue<ACTOR_MAXIMUM>>,
    current_slot_index: AtomicUsize,
    idle_frame_address: AtomicUsize,
    tick_count: AtomicU64,
}

impl CoreLocal {
    const fn new(core_index: usize) -> Self {
        Self {
            core_index,
            is_online: AtomicBool::new(false),
            run_queue: SpinLock::new(RunQueue::new()),
            current_slot_index: AtomicUsize::new(NO_ACTOR),
            idle_frame_address: AtomicUsize::new(0),
            tick_count: AtomicU64::new(0),
        }
    }

    /// The kernel's index for this core.
    #[must_use]
    pub const fn core_index(&self) -> usize {
        self.core_index
    }

    /// Whether the core has registered itself and is running kernel code.
    #[must_use]
    pub fn is_online(&self) -> bool {
        self.is_online.load(Ordering::Acquire)
    }

    /// Actors waiting to run on this core.
    #[must_use]
    pub const fn run_queue(&self) -> &SpinLock<RunQueue<ACTOR_MAXIMUM>> {
        &self.run_queue
    }

    /// The slot of the actor running here, or `NO_ACTOR`.
    #[must_use]
    pub fn current_slot_index(&self) -> usize {
        self.current_slot_index.load(Ordering::Acquire)
    }

    /// Records which actor runs here.
    pub fn set_current_slot_index(&self, slot_index: usize) {
        self.current_slot_index.store(slot_index, Ordering::Release);
    }

    /// The frame this core falls back to when it has no actor to run.
    #[must_use]
    pub fn idle_frame_address(&self) -> usize {
        self.idle_frame_address.load(Ordering::Acquire)
    }

    /// Records the frame this core falls back to when it has no actor to run.
    pub fn set_idle_frame_address(&self, frame_address: usize) {
        self.idle_frame_address
            .store(frame_address, Ordering::Release);
    }

    /// Counts a timer tick on this core and returns the new total.
    pub fn increment_tick_count(&self) -> u64 {
        self.tick_count.fetch_add(1, Ordering::Relaxed) + 1
    }
}

static CORE_LOCALS: [CoreLocal; CORE_MAXIMUM] = [
    CoreLocal::new(0),
    CoreLocal::new(1),
    CoreLocal::new(2),
    CoreLocal::new(3),
    CoreLocal::new(4),
    CoreLocal::new(5),
    CoreLocal::new(6),
    CoreLocal::new(7),
];

/// Every core's data.
#[must_use]
pub fn core_locals() -> &'static [CoreLocal; CORE_MAXIMUM] {
    &CORE_LOCALS
}

/// Makes the calling core known to the kernel as `core_index`. Returns `false` for an unknown index.
///
/// Each core calls this once, as early as possible after it starts.
pub fn register_current_core(core_index: usize) -> bool {
    let Some(core_local) = CORE_LOCALS.get(core_index) else {
        return false;
    };
    write_current_core_index(core_index);
    core_local.is_online.store(true, Ordering::Release);
    true
}

/// The calling core's index.
#[must_use]
pub fn current_core_index() -> usize {
    read_current_core_index().unwrap_or(usize::MAX)
}

/// The calling core's data, or `None` before the core has registered.
#[must_use]
pub fn current_core_local() -> Option<&'static CoreLocal> {
    CORE_LOCALS.get(current_core_index())
}

/// How many cores have registered themselves.
#[must_use]
pub fn online_core_count() -> usize {
    CORE_LOCALS
        .iter()
        .filter(|core_local| core_local.is_online())
        .count()
}
