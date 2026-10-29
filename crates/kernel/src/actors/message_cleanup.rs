//! # Actor Message Cleanup
//!
//! Coordinates actor termination with kernel-owned inbox and message-pool
//! resources. The actor-table lock protects inbox state; the message-pool lock
//! protects payload allocations. **The two locks are never held together.**
//!
//! Removing a message from a closed inbox does not make the actor recyclable:
//! `pending_message_releases` remains charged until the message-pool allocation
//! has been released. An outstanding receive lease likewise prevents recycling.
//!
//! This module does **not** remove actors from scheduler queues, free kernel
//! stacks, destroy address spaces, or advance actor generations. Those are
//! responsibilities of the actor lifecycle and scheduler. Callers may reuse
//! an actor slot only after both message cleanup and scheduler quiescence.

use foundation::address::LocalAddress;

use super::{
    control_block::{ActorState, ControlBlock},
    message_pool::{MessageHandle, MessagePool, PoolError, ReleasedMessage},
    table::with_control_block,
};
use crate::synchronization::spin_lock::SpinLock;

/// Why a requested actor message-cleanup operation could not proceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupError {
    /// The actor address refers to no table slot.
    InvalidSlot,
    /// The address's generation no longer matches the slot's actor.
    StaleAddress,
    /// The slot is already unoccupied.
    FreeActor,
    /// The actor has not entered its exiting state.
    NotExiting,
    /// The actor has no initialized inbox.
    MissingInbox,
    /// The inbox has not been closed against concurrent senders.
    InboxOpen,
    /// In-flight cleanup accounting overflowed or was unexpectedly empty.
    InvalidReleaseCount,
}

/// A message handle removed from a dying actor's inbox, still pending pool release.
///
/// This value is intentionally not `Copy` or `Clone`. Its owner's table slot
/// remains non-recyclable while the release is outstanding.
#[derive(Debug)]
#[must_use = "release this message and settle its in-flight cleanup charge"]
pub struct PendingMessageRelease {
    actor_address: LocalAddress,
    handle: MessageHandle,
}

impl PendingMessageRelease {
    /// Returns the actor incarnation whose cleanup owns this release.
    #[must_use]
    pub const fn actor_address(&self) -> LocalAddress {
        self.actor_address
    }
}

/// Outcome of one attempt to drain and reclaim a message from a closed inbox.
#[derive(Debug)]
#[must_use]
pub enum ReclaimResult<Reference, const MAX_REFERENCES: usize> {
    /// No queued messages remain; receive leases and prior releases may still exist.
    Empty,
    /// One pool allocation was freed, yielding its retained references for disposal.
    ///
    /// Dispose of these references outside actor-table and message-pool locks.
    Reclaimed(ReleasedMessage<Reference, MAX_REFERENCES>),
}

/// Failure of a message reclamation operation.
#[derive(Debug)]
#[must_use]
pub enum ReclaimError<Reference, const MAX_REFERENCES: usize> {
    /// The actor's state, identity, or inbox prevented reclamation.
    State(CleanupError),
    /// The pool rejected the handle; the caller retains it and must retry or
    /// quarantine the actor rather than recycling its slot.
    Pool {
        /// Pool validation error.
        error: PoolError,
        /// Owned message handle, still charged as an in-flight release.
        pending: PendingMessageRelease,
    },
    /// The pool released the handle, but the actor's bookkeeping could not
    /// be settled. Keep the slot quarantined until the discrepancy is resolved.
    Completion {
        /// Actor bookkeeping error.
        error: CleanupError,
        /// Retained references requiring release outside all locks.
        released: ReleasedMessage<Reference, MAX_REFERENCES>,
    },
}

/// Performs a table operation only when the supplied actor incarnation is live.
fn with_actor<Output>(
    actor_address: LocalAddress,
    operation: impl FnOnce(&mut ControlBlock) -> Result<Output, CleanupError>,
) -> Result<Output, CleanupError> {
    let slot_index =
        usize::try_from(actor_address.slot_index()).map_err(|_| CleanupError::InvalidSlot)?;
    with_control_block(slot_index, |control_block| {
        if control_block.state == ActorState::Free {
            return Err(CleanupError::FreeActor);
        }
        if control_block.generation_counter != actor_address.generation_counter() {
            return Err(CleanupError::StaleAddress);
        }
        operation(control_block)
    })
    .ok_or(CleanupError::InvalidSlot)?
}

/// Atomically marks an actor exiting and closes its inbox under its slot lock.
///
/// The caller must have coordinated with the scheduler to prevent this
/// actor from being scheduled again. This operation is safe for the actor's
/// inbox, but it does NOT itself establish scheduler quiescence.
/// This function only prevents further message admission; it does **not**
/// deschedule an actor. Repeated calls on the same exiting actor are safe.
///
/// # Errors
///
/// Rejects invalid or stale actor addresses and actors without an inbox.
pub fn begin_actor_message_exit(actor_address: LocalAddress) -> Result<(), CleanupError> {
    with_actor(actor_address, |control_block| {
        let inbox = control_block
            .inbox
            .as_mut()
            .ok_or(CleanupError::MissingInbox)?;
        inbox.close();
        control_block.state = ActorState::Exiting;
        Ok(())
    })
}

/// Removes the next queued envelope, retaining an in-flight cleanup charge.
///
/// The handle must be released through [`release_pending_message`] before this
/// actor is eligible for message cleanup completion. No pool lock is acquired
/// while the actor lock is held.
///
/// # Errors
///
/// Rejects any actor that is not exiting or whose inbox remains open.
pub fn take_next_closed_message(
    actor_address: LocalAddress,
) -> Result<Option<PendingMessageRelease>, CleanupError> {
    with_actor(actor_address, |control_block| {
        if control_block.state != ActorState::Exiting {
            return Err(CleanupError::NotExiting);
        }
        let inbox = control_block
            .inbox
            .as_mut()
            .ok_or(CleanupError::MissingInbox)?;
        if !inbox.is_closed() {
            return Err(CleanupError::InboxOpen);
        }

        // Check before removing anything so failure never loses ownership.
        let next_count = control_block
            .pending_message_releases
            .checked_add(1)
            .ok_or(CleanupError::InvalidReleaseCount)?;
        let Some(envelope) = inbox.drain_closed() else {
            return Ok(None);
        };
        control_block.pending_message_releases = next_count;
        Ok(Some(PendingMessageRelease {
            actor_address,
            handle: envelope.descriptor,
        }))
    })
}

/// Releases a previously drained message and settles its in-flight count.
///
/// The pool lock is released **before** reacquiring the actor-table lock.
/// Failure to release returns the owned handle and keeps the actor quarantined;
/// the caller must not discard it. Successful releases return retained
/// references for disposal after all locks have been released.
///
/// # Errors
///
/// Returns a pool error with its original handle, or a completion error with
/// the released resources if the actor table no longer matches expectations.
pub fn release_pending_message<
    Reference,
    const CAPACITY: usize,
    const MAX_PAYLOAD_BYTES: usize,
    const MAX_REFERENCES: usize,
>(
    pool: &SpinLock<MessagePool<Reference, CAPACITY, MAX_PAYLOAD_BYTES, MAX_REFERENCES>>,
    pending: PendingMessageRelease,
) -> Result<ReleasedMessage<Reference, MAX_REFERENCES>, ReclaimError<Reference, MAX_REFERENCES>> {
    let actor_address = pending.actor_address;
    if pending.handle.owner() != actor_address {
        return Err(ReclaimError::Pool {
            error: PoolError::WrongOwner,
            pending,
        });
    }
    let release_result = {
        let mut guard = pool.lock();
        guard.release(pending.handle)
    };
    let released = match release_result {
        Ok(released) => released,
        Err((error, handle)) => {
            return Err(ReclaimError::Pool {
                error,
                pending: PendingMessageRelease {
                    actor_address,
                    handle,
                },
            });
        }
    };

    let completion = with_actor(actor_address, |control_block| {
        if control_block.state != ActorState::Exiting {
            return Err(CleanupError::NotExiting);
        }
        control_block.pending_message_releases = control_block
            .pending_message_releases
            .checked_sub(1)
            .ok_or(CleanupError::InvalidReleaseCount)?;
        Ok(())
    });
    match completion {
        Ok(()) => Ok(released),
        Err(error) => Err(ReclaimError::Completion { error, released }),
    }
}

/// Drains and reclaims at most one queued message, without nested locks.
///
/// Call repeatedly until [`ReclaimResult::Empty`], disposing of returned
/// references outside all locks. A drained-but-unreleased handle is never
/// silently forgotten when the pool reports an error.
///
/// # Errors
///
/// Propagates actor state and pool errors while retaining ownership needed
/// for diagnosis or recovery.
pub fn reclaim_one_closed_message<
    Reference,
    const CAPACITY: usize,
    const MAX_PAYLOAD_BYTES: usize,
    const MAX_REFERENCES: usize,
>(
    actor_address: LocalAddress,
    pool: &SpinLock<MessagePool<Reference, CAPACITY, MAX_PAYLOAD_BYTES, MAX_REFERENCES>>,
) -> Result<ReclaimResult<Reference, MAX_REFERENCES>, ReclaimError<Reference, MAX_REFERENCES>> {
    let Some(pending) = take_next_closed_message(actor_address).map_err(ReclaimError::State)?
    else {
        return Ok(ReclaimResult::Empty);
    };
    release_pending_message(pool, pending).map(ReclaimResult::Reclaimed)
}

/// Indicates whether the actor's message-specific resources are fully settled.
///
/// A `true` result is **not** permission to reuse the actor-table slot by
/// itself: the scheduler must also prove the actor is neither executing nor
/// referenced by runnable, waiting, or deferred-wakeup queues. Only after
/// those checks may the lifecycle destroy memory and advance the generation.
///
/// # Errors
///
/// Rejects invalid, stale, non-exiting, or uninitialized actor slots.
pub fn message_cleanup_complete(actor_address: LocalAddress) -> Result<bool, CleanupError> {
    with_actor(actor_address, |control_block| {
        if control_block.state != ActorState::Exiting {
            return Err(CleanupError::NotExiting);
        }

        let Some(inbox) = control_block.inbox.as_ref() else {
            return Ok(control_block.pending_message_releases == 0);
        };

        Ok(control_block.pending_message_releases == 0 && inbox.can_recycle())
    })
}

/// Reclaims every queued message from a previously closed actor inbox.
///
/// `on_released` receives each set of retained capabilities **after** the
/// message-pool lock and actor-table lock have both been released. The caller
/// must dispose of those capabilities before returning; simply forgetting
/// them would leak kernel-owned resources.
///
/// This operation does not resolve a receive lease that is currently being
/// copied to user memory. Such a lease must be committed or cancelled by its
/// owning receive operation before the inbox can be detached.
///
/// # Errors
///
/// Returns [`ReclaimError`] if a pool release fails or the actor is no longer
/// exiting. A failed pool release retains its handle and in-flight accounting
/// in the returned error; callers must not recycle the actor slot.
pub fn reclaim_all_closed_messages<
    Reference,
    const CAPACITY: usize,
    const MAX_PAYLOAD_BYTES: usize,
    const MAX_REFERENCES: usize,
>(
    actor_address: LocalAddress,
    pool: &SpinLock<MessagePool<Reference, CAPACITY, MAX_PAYLOAD_BYTES, MAX_REFERENCES>>,
    mut on_released: impl FnMut(ReleasedMessage<Reference, MAX_REFERENCES>),
) -> Result<usize, ReclaimError<Reference, MAX_REFERENCES>> {
    let mut reclaimed_count = 0_usize;
    loop {
        match reclaim_one_closed_message(actor_address, pool)? {
            ReclaimResult::Empty => return Ok(reclaimed_count),
            ReclaimResult::Reclaimed(released) => {
                // All locks have been dropped by reclaim_one_closed_message.
                on_released(released);
                reclaimed_count += 1;
            }
        }
    }
}

/// Detaches an empty, closed inbox after its pool releases are fully settled.
///
/// Returns `Ok(false)` when a receive lease or a message-pool release is still
/// outstanding. The actor remains `Exiting` on both success and failure.
///
/// **This is not actor-slot recycling.** The scheduler must separately prove
/// that the actor is not executing and that no runnable, waiting, or pending
/// wake references remain before the address space or table slot is reclaimed.
///
/// # Errors
///
/// Returns [`CleanupError`] for invalid identity, unexpected actor state, or
/// missing inbox state. The check and removal occur under the same actor lock.
pub fn detach_reclaimed_inbox(actor_address: LocalAddress) -> Result<bool, CleanupError> {
    let detached = with_actor(actor_address, |control_block| {
        if control_block.state != ActorState::Exiting {
            return Err(CleanupError::NotExiting);
        }

        let Some(inbox) = control_block.inbox.as_ref() else {
            return Ok((control_block.pending_message_releases == 0, None)); // already detached
        };

        if control_block.pending_message_releases != 0 || !inbox.can_recycle() {
            return Ok((false, None));
        }

        Ok((true, control_block.inbox.take()))
    })?;

    drop(detached.1);
    Ok(detached.0)
}

/// Drains all queued messages and detaches the inbox when cleanup is complete.
///
/// Call this only after [`begin_actor_message_exit`] has closed the inbox.
/// Returns `Ok(false)` if an outstanding receive lease or release prevents
/// detachment. Returning `Ok(true)` leaves the actor in `Exiting`; this
/// function never frees its address space or publishes a free actor slot.
///
/// All released capabilities are passed to `on_released` outside locks.
///
/// # Errors
///
/// Propagates errors from pool reclamation and actor identity validation.
/// A failure must be handled before the actor can be recycled.
pub fn finish_actor_message_cleanup<
    Reference,
    const CAPACITY: usize,
    const MAX_PAYLOAD_BYTES: usize,
    const MAX_REFERENCES: usize,
>(
    actor_address: LocalAddress,
    pool: &SpinLock<MessagePool<Reference, CAPACITY, MAX_PAYLOAD_BYTES, MAX_REFERENCES>>,
    on_released: impl FnMut(ReleasedMessage<Reference, MAX_REFERENCES>),
) -> Result<bool, ReclaimError<Reference, MAX_REFERENCES>> {
    reclaim_all_closed_messages(actor_address, pool, on_released)?;
    detach_reclaimed_inbox(actor_address).map_err(ReclaimError::State)
}
