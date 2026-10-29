//! # Kernel Message Pool
//!
//! Stores bounded, kernel-owned message payloads and retained capability
//! references separately from each actor's selective-receive inbox.
//!
//! Each allocation is owned by a [`LocalAddress`], which includes the actor's
//! slot and generation. Message pool slots maintain independent generations.

use core::{array, mem};

use foundation::address::LocalAddress;

use super::inbox::{Envelope, MessageCharge};

/// Opaque, single-ownership reference to one live kernel message allocation.
///
/// Handles are intentionally neither `Copy` nor `Clone`. They are generation
/// checked on every pool operation to reject uses after slot recycling.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "release the message through its owning pool"]
pub struct MessageHandle {
    slot_index: usize,
    generation: u64,
    owner: LocalAddress,
}

impl MessageHandle {
    /// Returns the actor incarnation that owns this message allocation.
    pub const fn owner(&self) -> LocalAddress {
        self.owner
    }

    /// Returns the message's pool slot, for debugging and diagnostics only.
    pub const fn slot_index(&self) -> usize {
        self.slot_index
    }
}

/// The reason a pool operation could not be completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolError {
    /// No pool slot is available; global message backpressure is required.
    Exhausted,
    /// The supplied payload exceeds the pool's per-message size limit.
    PayloadTooLarge,
    /// A pool handle points outside this pool's slot array.
    InvalidHandle,
    /// A slot has been freed, retired, or reused under another generation.
    StaleHandle,
    /// The slot does not belong to the actor incarnation in the handle.
    WrongOwner,
    /// The destination scratch buffer is too small; no bytes were copied.
    BufferTooSmall,
    /// Calculating the conservative inbox charge overflowed `usize`.
    ChargeOverflow,
}

/// Failed allocation, returning all retained references to the caller.
///
/// Callers must release or otherwise dispose of the returned references
/// outside the pool lock; failure never transfers reference ownership.
#[derive(Debug)]
#[must_use]
pub struct AllocationFailure<Reference, const MAX_REFERENCES: usize> {
    /// The reason the allocation was rejected.
    pub error: PoolError,
    /// Unmodified references whose ownership remains with the caller.
    pub references: [Option<Reference>; MAX_REFERENCES],
}

/// Resources returned when a live message is released successfully.
///
/// The references must be decremented/released outside all kernel actor and
/// pool locks. `payload_length` is informational; payload bytes are zeroed.
#[derive(Debug)]
#[must_use]
pub struct ReleasedMessage<Reference, const MAX_REFERENCES: usize> {
    /// Kernel-retained references transferred out of the pool slot.
    pub references: [Option<Reference>; MAX_REFERENCES],
    /// Number of payload bytes previously stored in the released message.
    pub payload_length: usize,
}

/// Diagnostic counts for the statically provisioned pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolStatistics {
    /// Number of pool slots currently owned by live messages.
    pub allocated: usize,
    /// Number of free slots that can still be allocated.
    pub available: usize,
    /// Number of permanently retired slots whose generations exhausted.
    pub retired: usize,
    /// Conservative total bytes charged for currently allocated messages.
    pub charged_bytes: usize,
}

/// One slot in the statically allocated message pool.
struct MessageSlot<Reference, const MAX_PAYLOAD_BYTES: usize, const MAX_REFERENCES: usize> {
    generation: u64,
    occupied: bool,
    retired: bool,
    owner: Option<LocalAddress>,
    payload_length: usize,
    charge_bytes: usize,
    payload: [u8; MAX_PAYLOAD_BYTES],
    references: [Option<Reference>; MAX_REFERENCES],
}

impl<Reference, const MAX_PAYLOAD_BYTES: usize, const MAX_REFERENCES: usize>
    MessageSlot<Reference, MAX_PAYLOAD_BYTES, MAX_REFERENCES>
{
    /// Builds one empty slot with a fresh generation and zeroed storage.
    const fn new() -> Self {
        Self {
            generation: 1,
            occupied: false,
            retired: false,
            owner: None,
            payload_length: 0,
            charge_bytes: 0,
            payload: [0; MAX_PAYLOAD_BYTES],
            references: [const { None }; MAX_REFERENCES],
        }
    }
}

/// An externally synchronized, bounded pool of message payloads and references.
///
/// `Reference` must be a **kernel-retained capability**, not an arbitrary
/// user-supplied numeric reference. `CAPACITY` and `MAX_PAYLOAD_BYTES` bound
/// the memory footprint; `MAX_REFERENCES` bounds per-message attachments.
/// Prefer a single statically initialised pool protected by JAM's `SpinLock`.
///
/// Callers must not release a message while it is reserved by an in-flight
/// receive. This lifetime is enforced by the actor inbox's receive leases.
pub struct MessagePool<
    Reference,
    const CAPACITY: usize,
    const MAX_PAYLOAD_BYTES: usize,
    const MAX_REFERENCES: usize,
> {
    slots: [MessageSlot<Reference, MAX_PAYLOAD_BYTES, MAX_REFERENCES>; CAPACITY],
    allocated: usize,
    charged_bytes: usize,
}

impl<Reference, const CAPACITY: usize, const MAX_PAYLOAD_BYTES: usize, const MAX_REFERENCES: usize>
    MessagePool<Reference, CAPACITY, MAX_PAYLOAD_BYTES, MAX_REFERENCES>
{
    /// Creates an empty pool without heap allocation or runtime initialization.
    ///
    /// The const constructor allows placement into static `.bss`/`.data`.
    /// Prefer static placement for large capacities to avoid stack exhaustion.
    pub const fn new() -> Self {
        Self {
            slots: [const { MessageSlot::new() }; CAPACITY],
            allocated: 0,
            charged_bytes: 0,
        }
    }

    /// Returns the maximum number of simultaneously live messages.
    pub const fn capacity(&self) -> usize {
        CAPACITY
    }

    /// Returns the maximum number of payload bytes allowed per message.
    pub const fn maximum_payload_bytes(&self) -> usize {
        MAX_PAYLOAD_BYTES
    }

    /// Returns the number of currently allocated message slots.
    pub const fn allocated_count(&self) -> usize {
        self.allocated
    }

    /// Returns the conservative per-message charge for the given sender type.
    ///
    /// The fixed-slot accounting is intentional; an empty message still
    /// occupies the full statically provisioned message slot. The sender
    /// type is part of the charge because different address representations
    /// can produce different inbox descriptor sizes.
    pub fn message_charge<Sender>() -> Option<MessageCharge> {
        MessageCharge::checked_new(
            mem::size_of::<MessageSlot<Reference, MAX_PAYLOAD_BYTES, MAX_REFERENCES>>(),
            mem::size_of::<Envelope<MessageHandle, Sender>>(),
            0,
        )
    }

    /// Allocates a message for `owner` from already validated kernel data.
    ///
    /// `Sender` must match the `Envelope` sender type used by the inbox.
    /// Successful allocation takes ownership of all `references`; on failure,
    /// the returned [`AllocationFailure`] restores them unchanged. `owner`
    /// must have been verified against a live actor-table slot by the caller.
    /// If inbox admission subsequently fails, release the returned handle.
    ///
    /// Do not call while holding an actor slot lock, and do not pass a slice
    /// pointing into unvalidated EL0 memory or into a pool slot that could be
    /// mutated by allocation.
    pub fn allocate<Sender>(
        &mut self,
        owner: LocalAddress,
        payload: &[u8],
        references: [Option<Reference>; MAX_REFERENCES],
    ) -> Result<(MessageHandle, MessageCharge), AllocationFailure<Reference, MAX_REFERENCES>> {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(AllocationFailure {
                error: PoolError::PayloadTooLarge,
                references,
            });
        }
        let Some(charge) = Self::message_charge::<Sender>() else {
            return Err(AllocationFailure {
                error: PoolError::ChargeOverflow,
                references,
            });
        };
        let Some(new_charged_bytes) = self.charged_bytes.checked_add(charge.bytes()) else {
            return Err(AllocationFailure {
                error: PoolError::ChargeOverflow,
                references,
            });
        };
        let Some((slot_index, slot)) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| !slot.occupied && !slot.retired)
        else {
            return Err(AllocationFailure {
                error: PoolError::Exhausted,
                references,
            });
        };
        // Commit point: no fallible operation occurs once this slot is chosen.
        slot.payload[..payload.len()].copy_from_slice(payload);
        slot.payload_length = payload.len();
        slot.charge_bytes = charge.bytes();
        slot.owner = Some(owner);
        slot.references = references;
        slot.occupied = true;
        self.allocated += 1;
        self.charged_bytes = new_charged_bytes;
        Ok((
            MessageHandle {
                slot_index,
                generation: slot.generation,
                owner,
            },
            charge,
        ))
    }

    /// Copies a live message into caller-owned **kernel** scratch memory.
    ///
    /// Returns the number of copied payload bytes. If the scratch buffer is
    /// too small, no bytes are copied. User memory must be validated and copied
    /// separately after releasing the pool lock. The inbox receive lease must
    /// remain active throughout this operation and any subsequent EL0 copyout.
    pub fn copy_payload(
        &self,
        handle: &MessageHandle,
        kernel_scratch: &mut [u8],
    ) -> Result<usize, PoolError> {
        let slot = self.valid_slot(handle)?;
        if kernel_scratch.len() < slot.payload_length {
            return Err(PoolError::BufferTooSmall);
        }
        let length = slot.payload_length;
        kernel_scratch[..length].copy_from_slice(&slot.payload[..length]);
        Ok(length)
    }

    /// Borrows the references of a live message for kernel-only inspection.
    ///
    /// The returned borrow is valid only while the external pool lock remains
    /// held. Do not invoke user copyout, actor scheduler operations, callbacks,
    /// or resource release while holding that lock. References are not cloned
    /// or transferred by this method.
    pub fn references(
        &self,
        handle: &MessageHandle,
    ) -> Result<&[Option<Reference>; MAX_REFERENCES], PoolError> {
        Ok(&self.valid_slot(handle)?.references)
    }

    /// Releases a live message, invalidating its handle and clearing payload.
    ///
    /// Returns all retained references for cleanup outside the pool lock.
    /// The handle is consumed; a rejected release returns it unchanged so the
    /// caller can diagnose or recover. Slots whose generation reaches `u64::MAX`
    /// are permanently retired to prevent an ABA reuse after wraparound.
    ///
    /// A caller must hold the handle until an inbox delivery lease completes,
    /// and must not hold an actor lock while calling this method.
    pub fn release(
        &mut self,
        handle: MessageHandle,
    ) -> Result<ReleasedMessage<Reference, MAX_REFERENCES>, (PoolError, MessageHandle)> {
        if let Err(error) = self.valid_slot(&handle) {
            return Err((error, handle));
        }
        let slot = &mut self.slots[handle.slot_index];
        let references = mem::replace(&mut slot.references, array::from_fn(|_| None));
        let payload_length = slot.payload_length;
        let charge_bytes = slot.charge_bytes;
        slot.payload[..payload_length].fill(0);
        slot.charge_bytes = 0;
        slot.payload_length = 0;
        slot.owner = None;
        slot.occupied = false;
        if let Some(next_generation) = slot.generation.checked_add(1) {
            slot.generation = next_generation;
        } else {
            slot.retired = true;
        }
        self.allocated -= 1;
        self.charged_bytes -= charge_bytes;
        Ok(ReleasedMessage {
            references,
            payload_length,
        })
    }

    /// Returns counts of occupied, available, and retired message slots.
    pub fn statistics(&self) -> PoolStatistics {
        let retired = self.slots.iter().filter(|slot| slot.retired).count();
        let available = CAPACITY - self.allocated - retired;
        PoolStatistics {
            allocated: self.allocated,
            available,
            retired,
            charged_bytes: self.charged_bytes,
        }
    }

    /// Validates a slot index, generation, and actor ownership as one operation.
    fn valid_slot(
        &self,
        handle: &MessageHandle,
    ) -> Result<&MessageSlot<Reference, MAX_PAYLOAD_BYTES, MAX_REFERENCES>, PoolError> {
        let slot = self
            .slots
            .get(handle.slot_index)
            .ok_or(PoolError::InvalidHandle)?;
        if !slot.occupied || slot.generation != handle.generation {
            return Err(PoolError::StaleHandle);
        }
        if slot.owner != Some(handle.owner) {
            return Err(PoolError::WrongOwner);
        }
        Ok(slot)
    }
}
