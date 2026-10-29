//! # Actor Inbox
//!
//! Bounded, kernel-managed message storage with selective receive, resource
//! accounting, and generation-safe notification tokens.
//!
//! Inbox ownership uses [`LocalAddress`], the canonical JAM actor identity.
//! Message payload storage belongs to the separate kernel message pool.
//!
//! All operations require exclusive access under the actor control-block lock.
//! Runtime-level payload matching and scheduler park/wake transitions are
//! outside this module's responsibility.

use core::array;
use foundation::address::LocalAddress;

/// Charge computed by the kernel, including descriptor, retained payload, and
/// any reference-accounting cost. The API prohibits zero-cost messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageCharge(usize);

impl MessageCharge {
    /// Computes the nonzero resident-memory charge for a message.
    ///
    /// Returns `None` if the sum overflows `usize` or every component is zero.
    pub fn checked_new(
        descriptor_bytes: usize,
        payload_bytes: usize,
        reference_bytes: usize,
    ) -> Option<Self> {
        let total = descriptor_bytes
            .checked_add(payload_bytes)?
            .checked_add(reference_bytes)?;
        if total == 0 { None } else { Some(Self(total)) }
    }

    /// Returns the total charged resident bytes.
    pub const fn bytes(self) -> usize {
        self.0
    }
}

/// `sender` is authenticated by the kernel's current actor context; `tag` and
/// `correlation` are untrusted application labels, NOT capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageMetadata<Sender> {
    /// Kernel-authenticated identity of the actor that sent this message.
    pub sender: Sender,
    /// Application-provided message tag, not a security capability.
    pub tag: u64,
    /// Application-provided correlation identifier, not a security capability.
    pub correlation: u64,
}

/// Generic descriptor: intended to be a handle to a separate message pool,
/// not a 4 KiB payload embedded in every actor's control block.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub struct Envelope<Descriptor, Sender> {
    /// Kernel-authenticated sender identity and untrusted application labels.
    pub metadata: MessageMetadata<Sender>,
    /// Handle to kernel-owned payload storage and references.
    pub descriptor: Descriptor,
    /// Resident-memory charge retained until delivery or disposal completes.
    pub charge: MessageCharge,
}

impl<Descriptor, Sender> Envelope<Descriptor, Sender> {
    /// Constructs an envelope after the caller has validated its metadata and
    /// calculated the charge for the referenced kernel-owned message.
    pub const fn new(
        metadata: MessageMetadata<Sender>,
        descriptor: Descriptor,
        charge: MessageCharge,
    ) -> Self {
        Self {
            metadata,
            descriptor,
            charge,
        }
    }
}

/// Select the oldest matching message, without removing earlier unmatched
/// messages. The runtime is responsible for full payload pattern matching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiveSelector<Sender> {
    /// If set, only messages from this authenticated sender match.
    pub sender: Option<Sender>,
    /// If set, only messages with this application tag match.
    pub tag: Option<u64>,
    /// If set, only messages with this correlation identifier match.
    pub correlation: Option<u64>,
}

impl<Sender: Copy + Eq> ReceiveSelector<Sender> {
    /// Creates a selector matching any message metadata.
    pub const fn any() -> Self {
        Self {
            sender: None,
            tag: None,
            correlation: None,
        }
    }

    /// Creates a selector matching the specified application tag.
    pub const fn tagged(tag: u64) -> Self {
        Self {
            sender: None,
            tag: Some(tag),
            correlation: None,
        }
    }

    /// Returns whether all specified metadata filters match this message.
    /// Payload matching remains the responsibility of the runtime.
    pub fn matches(&self, metadata: &MessageMetadata<Sender>) -> bool {
        self.sender.map_or(true, |sender| sender == metadata.sender)
            && self.tag.map_or(true, |tag| tag == metadata.tag)
            && self
                .correlation
                .map_or(true, |correlation| correlation == metadata.correlation)
    }
}

/// Reason an inbox rejected a message without taking ownership of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// Recipient is closing or has already terminated.
    Closed,
    /// No free descriptor slot is available.
    Full,
    /// Admitting the message would exceed its configured byte budget.
    BudgetExceeded,
    /// Monotonically increasing message identifiers have been exhausted.
    IdentifierExhausted,
}

/// A token is valid only for one actor incarnation and one wait attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct WakeToken {
    local_address: LocalAddress,
    wait_identifier: u128,
}

impl WakeToken {
    /// Returns the local local_address authorized by this notification.
    pub const fn actor_address(self) -> LocalAddress {
        self.local_address
    }
    /// Returns the identifier of the wait attempt this token represents.
    pub const fn wait_identifier(self) -> u128 {
        self.wait_identifier
    }
}

/// Scheduler action required after successfully inserting a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum SendEffect {
    /// The message was queued without requiring a scheduler wakeup.
    Enqueued,
    /// Must be submitted to the scheduler's generation-aware wake path.
    /// Calling this twice must never make the actor runnable twice.
    Wake(WakeToken),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeaseKey {
    local_address: LocalAddress,
    slot_index: usize,
    identifier: u128,
}

/// Owns the descriptor while user copy-out is in progress. Cannot be cloned.
/// Both message count and budget stay charged until this is resolved.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub struct ReceiveLease<Descriptor, Sender> {
    key: LeaseKey,
    sequence: u128,
    envelope: Envelope<Descriptor, Sender>,
}

impl<Descriptor, Sender> ReceiveLease<Descriptor, Sender> {
    /// Inspection does not transfer or permit mutation of the held descriptor.
    pub const fn envelope(&self) -> &Envelope<Descriptor, Sender> {
        &self.envelope
    }

    /// Continue scanning after this message when its application payload does
    /// not match. Cancel the lease before starting the next scan.
    pub const fn cursor(&self) -> ReceiveCursor {
        ReceiveCursor {
            local_address: self.key.local_address,
            sequence: self.sequence,
        }
    }
}

/// Position of a previously examined message within one actor incarnation.
/// Used to continue selective scans without consuming rejected messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiveCursor {
    local_address: LocalAddress,
    sequence: u128,
}

/// Outcome of selecting a message or registering a receive wait.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum ReceiveResult<Descriptor, Sender> {
    /// Temporarily owns the oldest matching envelope for attempted copy-out.
    Leased(ReceiveLease<Descriptor, Sender>),
    /// Wait registration succeeded; the scheduler may attempt to park the actor.
    Armed(WakeToken),
    /// A receive lease or outstanding wait prevents another receive attempt.
    Busy,
    /// The inbox has closed and will not deliver further messages.
    Closed,
    /// No match and all descriptor slots or available bytes are exhausted.
    /// Waiting here could deadlock without the runtime draining messages.
    Saturated,
    /// The cursor belongs to a different actor incarnation.
    InvalidCursor,
    /// Monotonic receive-lease or wait identifiers have been exhausted.
    IdentifierExhausted,
}

/// Outcome of finalizing a receive wait before descheduling an actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum ParkResult {
    /// Scheduler may commit BLOCKED only under its own run-state lock.
    Parked,
    /// Matching send raced with park: stay on this core and retry receive.
    Retry,
    /// The token no longer names the currently armed wait.
    InvalidToken,
    /// The actor inbox was closed before the wait could finish.
    Closed,
}

/// Error returned when completing or cancelling an invalid receive lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveError {
    /// The lease is stale, belongs to another actor, or was already resolved.
    InvalidLease,
}

/// Consumed by the caller outside the actor lock. A completed copy-out after
/// actor closure must be discarded because the destination has terminated.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum CompleteResult<Descriptor, Sender> {
    /// Copy-out completed and the caller must finalize ownership outside the lock.
    Delivered(Envelope<Descriptor, Sender>),
    /// Inbox closed during copy-out; the caller must discard the envelope safely.
    Discard(Envelope<Descriptor, Sender>),
}

/// Outcome of cancelling a receive lease after a failed copy-out or mismatch.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum CancelResult<Descriptor, Sender> {
    /// Original message restored to its position in arrival order.
    Restored,
    /// Inbox closed during copy-out; caller must dispose of the envelope.
    Discard(Envelope<Descriptor, Sender>),
}

enum Slot<Descriptor, Sender> {
    Vacant,
    Queued {
        sequence: u128,
        envelope: Envelope<Descriptor, Sender>,
    },
    Leased {
        sequence: u128,
        identifier: u128,
        charge_bytes: usize,
    },
}

#[derive(Clone, Copy)]
enum WaitState<Sender> {
    None,
    Armed {
        token: WakeToken,
        selector: ReceiveSelector<Sender>,
    },
    Signaled {
        token: WakeToken,
    },
    Parked {
        token: WakeToken,
        selector: ReceiveSelector<Sender>,
    },
    WakeQueued {
        token: WakeToken,
    },
}

/// `CAPACITY` is a message-count quota; `maximum_resident_bytes` is a separate
/// byte quota. Storage and selection are O(CAPACITY), avoiding indexes that
/// could themselves become inconsistent under termination or rollback.
///
/// All methods require EXCLUSIVE access protected by the actor control-block
/// lock. This struct contains no synchronization primitives.
pub struct Inbox<Descriptor, Sender, const CAPACITY: usize> {
    local_address: LocalAddress,
    slots: [Slot<Descriptor, Sender>; CAPACITY],
    message_count: usize,
    resident_bytes: usize,
    maximum_resident_bytes: usize,
    next_message_sequence: u128,
    next_lease_identifier: u128,
    next_wait_identifier: u128,
    active_lease: Option<LeaseKey>,
    wait_state: WaitState<Sender>,
    closed: bool,
}

impl<Descriptor, Sender: Copy + Eq, const CAPACITY: usize> Inbox<Descriptor, Sender, CAPACITY> {
    /// A reused actor slot must use its NEW generation, with a fresh inbox.
    pub fn new(local_address: LocalAddress, maximum_resident_bytes: usize) -> Self {
        Self {
            local_address,
            slots: array::from_fn(|_| Slot::Vacant),
            message_count: 0,
            resident_bytes: 0,
            maximum_resident_bytes,
            next_message_sequence: 0,
            next_lease_identifier: 0,
            next_wait_identifier: 0,
            active_lease: None,
            wait_state: WaitState::None,
            closed: false,
        }
    }

    /// Number of accounted messages, including any actively leased message.
    pub const fn message_count(&self) -> usize {
        self.message_count
    }
    /// Total bytes charged to this inbox, including active leases.
    pub const fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }
    /// Returns whether this inbox no longer accepts messages.
    pub const fn is_closed(&self) -> bool {
        self.closed
    }
    /// Returns the local local_address of the actor that owns this inbox.
    pub const fn actor_address(&self) -> LocalAddress {
        self.local_address
    }

    /// A slot must not be reused while a descriptor is leased for copy-out.
    pub fn can_recycle(&self) -> bool {
        self.closed
            && self.message_count == 0
            && self.active_lease.is_none()
            && matches!(self.wait_state, WaitState::None)
    }

    /// Ownership of `envelope` transfers only on success. Failed admission
    /// returns the original envelope for exactly-once external cleanup.
    pub fn send(
        &mut self,
        envelope: Envelope<Descriptor, Sender>,
    ) -> Result<SendEffect, (SendError, Envelope<Descriptor, Sender>)> {
        if self.closed {
            return Err((SendError::Closed, envelope));
        }
        if self.message_count >= CAPACITY {
            return Err((SendError::Full, envelope));
        }
        let Some(next_total) = self.resident_bytes.checked_add(envelope.charge.bytes()) else {
            return Err((SendError::BudgetExceeded, envelope));
        };
        if next_total > self.maximum_resident_bytes {
            return Err((SendError::BudgetExceeded, envelope));
        }
        let Some(next_sequence) = self.next_message_sequence.checked_add(1) else {
            return Err((SendError::IdentifierExhausted, envelope));
        };
        let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| matches!(slot, Slot::Vacant))
        else {
            return Err((SendError::Full, envelope));
        };
        let effect = match self.wait_state {
            WaitState::Armed { token, selector } if selector.matches(&envelope.metadata) => {
                self.wait_state = WaitState::Signaled { token };
                SendEffect::Enqueued // waiter is still executing; do not queue it
            }
            WaitState::Parked { token, selector } if selector.matches(&envelope.metadata) => {
                self.wait_state = WaitState::WakeQueued { token };
                SendEffect::Wake(token)
            }
            _ => SendEffect::Enqueued,
        };
        *slot = Slot::Queued {
            sequence: self.next_message_sequence,
            envelope,
        };
        self.next_message_sequence = next_sequence;
        self.message_count += 1;
        self.resident_bytes = next_total;
        Ok(effect)
    }

    /// Returns a lease to the *oldest matching* envelope, or arms a wait.
    /// `Armed` does not mean that the actor has actually been descheduled.
    pub fn receive_or_arm(
        &mut self,
        selector: ReceiveSelector<Sender>,
    ) -> ReceiveResult<Descriptor, Sender> {
        self.receive_or_arm_after(selector, None)
    }

    /// After rejecting a candidate in userland, cancel its lease and retry
    /// with `Some(lease.cursor())` to visit subsequent messages in order.
    /// A cursor MUST come from this exact actor incarnation.
    pub fn receive_or_arm_after(
        &mut self,
        selector: ReceiveSelector<Sender>,
        after: Option<ReceiveCursor>,
    ) -> ReceiveResult<Descriptor, Sender> {
        if self.closed {
            return ReceiveResult::Closed;
        }
        if self.active_lease.is_some() || !matches!(self.wait_state, WaitState::None) {
            return ReceiveResult::Busy;
        }
        if let Some(cursor) = after {
            if cursor.local_address != self.local_address {
                return ReceiveResult::InvalidCursor;
            }
        }
        let mut earliest: Option<(usize, u128)> = None;
        for (index, slot) in self.slots.iter().enumerate() {
            if let Slot::Queued { sequence, envelope } = slot {
                if after.map_or(true, |cursor| *sequence > cursor.sequence)
                    && selector.matches(&envelope.metadata)
                    && earliest.map_or(true, |(_, prior)| *sequence < prior)
                {
                    earliest = Some((index, *sequence));
                }
            }
        }
        if let Some((slot_index, sequence)) = earliest {
            let Some(next_identifier) = self.next_lease_identifier.checked_add(1) else {
                return ReceiveResult::IdentifierExhausted;
            };
            let slot = &mut self.slots[slot_index];
            let Slot::Queued { envelope, .. } = core::mem::replace(slot, Slot::Vacant) else {
                unreachable!("selected an occupied slot under actor lock");
            };
            let key = LeaseKey {
                local_address: self.local_address,
                slot_index,
                identifier: self.next_lease_identifier,
            };
            *slot = Slot::Leased {
                sequence,
                identifier: key.identifier,
                charge_bytes: envelope.charge.bytes(),
            };
            self.active_lease = Some(key);
            self.next_lease_identifier = next_identifier;
            return ReceiveResult::Leased(ReceiveLease {
                key,
                sequence,
                envelope,
            });
        }
        // If a selective receive skips all stored messages, never park when
        // full: the awaited message could not be admitted without draining.
        if self.message_count == CAPACITY || self.resident_bytes >= self.maximum_resident_bytes {
            return ReceiveResult::Saturated;
        }
        let Some(next_identifier) = self.next_wait_identifier.checked_add(1) else {
            return ReceiveResult::IdentifierExhausted;
        };
        let token = WakeToken {
            local_address: self.local_address,
            wait_identifier: self.next_wait_identifier,
        };
        self.next_wait_identifier = next_identifier;
        self.wait_state = WaitState::Armed { token, selector };
        ReceiveResult::Armed(token)
    }

    /// Called in the scheduler's block transaction while holding the actor
    /// slot lock. `Parked` must be paired with the scheduler's own safe BLOCKED
    /// transition, with no chance of concurrent execution on another core.
    pub fn finish_park(&mut self, token: WakeToken) -> ParkResult {
        if self.closed {
            return ParkResult::Closed;
        }
        match self.wait_state {
            WaitState::Armed {
                token: current,
                selector,
            } if current == token => {
                self.wait_state = WaitState::Parked { token, selector };
                ParkResult::Parked
            }
            WaitState::Signaled { token: current } if current == token => {
                self.wait_state = WaitState::None;
                ParkResult::Retry
            }
            _ => ParkResult::InvalidToken,
        }
    }

    /// Scheduler validation of a queued wake. This method ONLY consumes the
    /// notification: the scheduler must separately synchronize its run-state
    /// and validate the actor's generation before marking it runnable.
    /// Stale or duplicate wake requests return false.
    pub fn claim_wake(&mut self, token: WakeToken) -> bool {
        match self.wait_state {
            WaitState::WakeQueued { token: current } if current == token && !self.closed => {
                self.wait_state = WaitState::None;
                true
            }
            _ => false,
        }
    }

    /// Abort a wait while the actor is still RUNNING or during termination.
    /// NOT a substitute for waking an actor already PARKED in the scheduler.
    pub fn cancel_armed_wait(&mut self, token: WakeToken) -> bool {
        if matches!(self.wait_state, WaitState::Armed { token: current, .. }
            | WaitState::Signaled { token: current } if current == token)
        {
            self.wait_state = WaitState::None;
            true
        } else {
            false
        }
    }

    /// `lease` is consumed, preventing double completion. Return its envelope
    /// after decrementing budget so the caller can finalize ownership OUTSIDE
    /// the actor lock. Caller must have successfully copied to EL0 first.
    pub fn complete_receive(
        &mut self,
        lease: ReceiveLease<Descriptor, Sender>,
    ) -> Result<CompleteResult<Descriptor, Sender>, (ReceiveError, ReceiveLease<Descriptor, Sender>)>
    {
        if !self.valid_lease(&lease) {
            return Err((ReceiveError::InvalidLease, lease));
        }
        self.release_lease_slot(lease.key, lease.envelope.charge.bytes());
        if self.closed {
            Ok(CompleteResult::Discard(lease.envelope))
        } else {
            Ok(CompleteResult::Delivered(lease.envelope))
        }
    }

    /// Copy-out failed: restore the original arrival position if the actor
    /// still exists, otherwise discard and release the envelope externally.
    pub fn cancel_receive(
        &mut self,
        lease: ReceiveLease<Descriptor, Sender>,
    ) -> Result<CancelResult<Descriptor, Sender>, (ReceiveError, ReceiveLease<Descriptor, Sender>)>
    {
        if !self.valid_lease(&lease) {
            return Err((ReceiveError::InvalidLease, lease));
        }
        let slot_index = lease.key.slot_index;
        if self.closed {
            self.release_lease_slot(lease.key, lease.envelope.charge.bytes());
            Ok(CancelResult::Discard(lease.envelope))
        } else {
            let sequence = match &self.slots[slot_index] {
                Slot::Leased { sequence, .. } => *sequence,
                _ => unreachable!("lease was validated under the actor lock"),
            };
            self.slots[slot_index] = Slot::Queued {
                sequence,
                envelope: lease.envelope,
            };
            self.active_lease = None;
            Ok(CancelResult::Restored)
        }
    }

    /// Makes inbox non-deliverable and cancels wait tokens. The caller MUST
    /// drain it and dispose of all returned envelopes outside the slot lock.
    pub fn close(&mut self) {
        self.closed = true;
        self.wait_state = WaitState::None;
    }

    /// Pull one queued envelope after `close`. The caller drops/returns refs
    /// outside the actor lock, then repeats. Leased envelopes must resolve
    /// before the table slot or local_address space is recycled.
    pub fn drain_closed(&mut self) -> Option<Envelope<Descriptor, Sender>> {
        if !self.closed {
            return None;
        }
        let mut earliest: Option<(usize, u128)> = None;
        for (index, slot) in self.slots.iter().enumerate() {
            if let Slot::Queued { sequence, .. } = slot {
                if earliest.map_or(true, |(_, prior)| *sequence < prior) {
                    earliest = Some((index, *sequence));
                }
            }
        }
        let (index, _) = earliest?;
        let Slot::Queued { envelope, .. } =
            core::mem::replace(&mut self.slots[index], Slot::Vacant)
        else {
            unreachable!("selected queued slot")
        };
        self.message_count -= 1;
        self.resident_bytes -= envelope.charge.bytes();
        Some(envelope)
    }

    fn valid_lease(&self, lease: &ReceiveLease<Descriptor, Sender>) -> bool {
        if self.active_lease != Some(lease.key) || lease.key.local_address != self.local_address {
            return false;
        }
        matches!(self.slots.get(lease.key.slot_index), Some(Slot::Leased {
            identifier, charge_bytes, ..
        }) if *identifier == lease.key.identifier && *charge_bytes == lease.envelope.charge.bytes())
    }

    fn release_lease_slot(&mut self, key: LeaseKey, charge_bytes: usize) {
        self.slots[key.slot_index] = Slot::Vacant;
        self.active_lease = None;
        self.message_count -= 1;
        self.resident_bytes -= charge_bytes;
    }
}
