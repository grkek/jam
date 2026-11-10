//! Data that belongs to exactly one core.
//!
//! Each core records its own index in `TPIDR_EL1` as it starts, so finding "my" data is
//! a register read and an array lookup, with no lock and no search.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::{
    architecture::aarch64::processor::{read_current_core_index, write_current_core_index},
    memory::{frame_allocator::FrameAllocator, paged_directory::DirectoryError},
    scheduling::queue::Queue,
    synchronization::spin_lock::SpinLock,
};

/// Most cores this kernel supports. Matches `CORE_COUNT` in the linker script.
pub const CORE_COUNT: usize = 16;
/// Stands in for a slot index when a core runs no actor.
pub const NO_ACTOR: usize = usize::MAX;

/// Independently sampled scheduler counters; they are diagnostics, not a coherent snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerStatistics {
    /// Approximate queued actor count; the owning queue lock is authoritative.
    pub queued_actor_count: usize,
    /// Timer ticks handled by this core.
    pub tick_count: u64,
    /// Successful actor dispatches (excluding the no-switch timer fast path).
    pub actor_dispatch_count: u64,
    /// Work-stealing scans initiated by this core.
    pub steal_attempt_count: u64,
    /// Runnable actors moved here from other cores.
    pub stolen_actor_count: u64,
    /// Dispatched actors whose previous core differs from this core.
    pub migration_count: u64,
    /// Times dispatch fell back to this core's idle context.
    pub idle_dispatch_count: u64,
}

/// One core's data, aligned to a cache line so cores never contend for the same line.
#[repr(align(64))]
pub struct CoreLocal {
    core_index: usize,
    is_online: AtomicBool,
    queue: SpinLock<Queue>,
    published_queue_length: AtomicUsize,
    current_slot_index: AtomicUsize,
    idle_frame_address: AtomicUsize,
    tick_count: AtomicU64,
    actor_dispatch_count: AtomicU64,
    steal_attempt_count: AtomicU64,
    stolen_actor_count: AtomicU64,
    migration_count: AtomicU64,
    idle_dispatch_count: AtomicU64,
}

impl CoreLocal {
    const fn new(core_index: usize) -> Self {
        Self {
            core_index,
            is_online: AtomicBool::new(false),
            queue: SpinLock::new(Queue::new()),
            published_queue_length: AtomicUsize::new(0),
            current_slot_index: AtomicUsize::new(NO_ACTOR),
            idle_frame_address: AtomicUsize::new(0),
            tick_count: AtomicU64::new(0),
            actor_dispatch_count: AtomicU64::new(0),
            steal_attempt_count: AtomicU64::new(0),
            stolen_actor_count: AtomicU64::new(0),
            migration_count: AtomicU64::new(0),
            idle_dispatch_count: AtomicU64::new(0),
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
    pub const fn queue(&self) -> &SpinLock<Queue> {
        &self.queue
    }

    /// Approximate run-queue depth, published while holding the run-queue lock.
    ///
    /// Consumers must recheck the queue under lock before modifying its contents.
    #[must_use]
    pub fn queue_length_estimate(&self) -> usize {
        self.published_queue_length.load(Ordering::Relaxed)
    }

    /// Publishes a queue length after a successful push, pop, or steal.
    ///
    /// The caller must hold this core's run-queue lock.
    pub fn publish_queue_length(&self, length: usize) {
        self.published_queue_length.store(length, Ordering::Relaxed);
    }

    /// Estimated load for soft wake-up placement, including a running actor.
    #[must_use]
    pub fn load_estimate(&self) -> usize {
        self.queue_length_estimate()
            .saturating_add(usize::from(self.current_slot_index() != NO_ACTOR))
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

    /// Records a successful actor dispatch and optional inter-core migration.
    pub fn record_actor_dispatch(&self, migrated: bool) {
        self.actor_dispatch_count.fetch_add(1, Ordering::Relaxed);
        if migrated {
            self.migration_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records an attempted steal scan; called only when balancing is requested.
    pub fn record_steal_attempt(&self) {
        self.steal_attempt_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a completed batch of stolen runnable actors.
    pub fn record_stolen_actors(&self, count: usize) {
        self.stolen_actor_count
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    /// Records a dispatch to this core's idle frame.
    pub fn record_idle_dispatch(&self) {
        self.idle_dispatch_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Reads the per-core scheduler counters without acquiring global locks.
    #[must_use]
    pub fn scheduler_statistics(&self) -> SchedulerStatistics {
        SchedulerStatistics {
            queued_actor_count: self.queue_length_estimate(),
            tick_count: self.tick_count.load(Ordering::Relaxed),
            actor_dispatch_count: self.actor_dispatch_count.load(Ordering::Relaxed),
            steal_attempt_count: self.steal_attempt_count.load(Ordering::Relaxed),
            stolen_actor_count: self.stolen_actor_count.load(Ordering::Relaxed),
            migration_count: self.migration_count.load(Ordering::Relaxed),
            idle_dispatch_count: self.idle_dispatch_count.load(Ordering::Relaxed),
        }
    }
}

/// Builds one entry per core, each knowing its own index.
const fn build_core_locals() -> [CoreLocal; CORE_COUNT] {
    let mut core_locals = [const { CoreLocal::new(0) }; CORE_COUNT];

    let mut core_index = 1;
    while core_index < CORE_COUNT {
        core_locals[core_index] = CoreLocal::new(core_index);
        core_index += 1;
    }

    core_locals
}

static CORE_LOCALS: [CoreLocal; CORE_COUNT] = build_core_locals();

/// Gives every core's queue room for `capacity` actors and returns how many frames that took.
///
/// An actor sits in at most one queue at a time, so the number of actor slots is enough
/// for every queue, even when work stealing moves entries between cores.
///
/// Call once at boot, while the caller still owns the frame allocator and before the first spawn.
///
/// # Errors
///
/// Returns [`DirectoryError`] when a queue cannot be built.
pub fn initialise_queues(
    frame_allocator: &mut FrameAllocator,
    capacity: usize,
) -> Result<usize, DirectoryError> {
    let mut frame_count = 0;

    for core_local in &CORE_LOCALS {
        frame_count += core_local
            .queue
            .lock()
            .initialise(frame_allocator, capacity)?;
    }

    Ok(frame_count)
}

/// Every core's data.
#[must_use]
pub fn core_locals() -> &'static [CoreLocal; CORE_COUNT] {
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
