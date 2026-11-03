//! Local copied-message system calls; register conventions live in foundation.
//!
//! Handlers execute with interrupts masked on the exception stack. Only the
//! current actor accesses its user buffers. Forced remote termination is not
//! implemented by the lifecycle; adding it requires pinning in-flight copies.

use super::actor_calls::{current_address, pack_local_address};
use crate::{
    actors::{
        control_block::{ActorState, ControlBlock},
        inbox::{
            CancelResult, CompleteResult, InboxWaitResult, MessageMetadata, ParkResult,
            ReceiveResult, ReceiveSelector,
        },
        message_storage::MESSAGE_POOL,
        table::{ACTOR_MAXIMUM, with_control_block},
    },
    architecture::aarch64::{exceptions::TrapFrame, power_interface::halt_machine},
    memory::user_copy::{copy_from_actor, copy_to_actor},
    messaging::delivery::{release_message, tell_actor as deliver_message},
    scheduling::{
        core_local::{NO_ACTOR, current_core_local},
        scheduler::pick_next_after_wait,
    },
};
use foundation::{
    address::LocalAddress,
    messaging::{
        INDEFINITE_WAIT, MATCH_CORRELATION, MATCH_SENDER, MATCH_TAG, MAXIMUM_PAYLOAD_BYTE_COUNT,
        MAXIMUM_RECORD_BYTE_COUNT, RECEIVED_HEADER_BYTE_COUNT, SELECTOR_FLAGS,
    },
    system_call_status::SystemCallStatus,
};

/// Decodes a packed local address, rejecting slots that cannot exist on this node.
fn local_address(packed: u64) -> Result<LocalAddress, SystemCallStatus> {
    let slot = u32::try_from(packed & u64::from(u32::MAX))
        .map_err(|_| SystemCallStatus::MalformedAddress)?;
    let generation = u32::try_from(packed >> 32).map_err(|_| SystemCallStatus::MalformedAddress)?;
    if slot as usize >= ACTOR_MAXIMUM {
        return Err(SystemCallStatus::MalformedAddress);
    }
    Ok(LocalAddress::new(slot, generation))
}

/// Resolves identity from the scheduler rather than from an actor argument.
fn caller() -> Result<LocalAddress, SystemCallStatus> {
    let (status, packed) = current_address();
    if status != SystemCallStatus::Accepted {
        return Err(status);
    }
    local_address(packed)
}

/// Applies an operation under the current incarnation's actor lock.
fn with_caller<Output>(
    address: LocalAddress,
    operation: impl FnOnce(&mut ControlBlock) -> Output,
) -> Output {
    with_control_block(address.slot_index() as usize, |actor| {
        if actor.state != ActorState::Running
            || actor.generation_counter != address.generation_counter()
        {
            halt_machine();
        }
        operation(actor)
    })
    .unwrap_or_else(|| halt_machine())
}

/// Copies all argument registers before any handler publishes a resumable frame.
fn arguments(trap_frame: *mut TrapFrame) -> [u64; 6] {
    // SAFETY: The dispatcher supplies the live, exclusively owned saved frame.
    let frame = unsafe { &*trap_frame };
    core::array::from_fn(|index| frame.general_registers.get(index).copied().unwrap_or(0))
}

/// Writes a result before the saved frame can become runnable on another core.
fn answer(trap_frame: *mut TrapFrame, status: SystemCallStatus, value: u64) {
    // SAFETY: The handler still owns this frame; parked frames are never passed here.
    let frame = unsafe { &mut *trap_frame };
    if let Some(register) = frame.general_registers.get_mut(0) {
        *register = status.number();
    }
    if let Some(register) = frame.general_registers.get_mut(1) {
        *register = value;
    }
}

/// Builds a validated metadata selector. Unused filter words are ignored.
fn selector(
    flags: u64,
    sender: u64,
    tag: u64,
    correlation: u64,
) -> Result<ReceiveSelector<LocalAddress>, SystemCallStatus> {
    if flags & !SELECTOR_FLAGS != 0 {
        return Err(SystemCallStatus::InvalidArgument);
    }
    Ok(ReceiveSelector {
        sender: if flags & MATCH_SENDER != 0 {
            Some(local_address(sender)?)
        } else {
            None
        },
        tag: (flags & MATCH_TAG != 0).then_some(tag),
        correlation: (flags & MATCH_CORRELATION != 0).then_some(correlation),
    })
}

/// Sends a bounded local payload using an authenticated sender identity.
#[must_use]
pub fn tell_actor(trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    let [destination, pointer, length, tag, correlation, reserved] = arguments(trap_frame);
    let result = (|| {
        if reserved != 0 {
            return Err(SystemCallStatus::UnsupportedOperation);
        }
        let destination = local_address(destination)?;
        let length = usize::try_from(length).map_err(|_| SystemCallStatus::InvalidArgument)?;
        if length > MAXIMUM_PAYLOAD_BYTE_COUNT {
            return Err(SystemCallStatus::InvalidArgument);
        }
        let pointer = usize::try_from(pointer).map_err(|_| SystemCallStatus::InvalidUserMemory)?;
        let address = caller()?;
        let mut payload = [0u8; MAXIMUM_PAYLOAD_BYTE_COUNT];
        let bytes = payload
            .get_mut(..length)
            .ok_or(SystemCallStatus::InvalidArgument)?;
        with_caller(address, |actor| {
            let space = actor
                .address_space
                .as_ref()
                .ok_or(SystemCallStatus::InvalidUserMemory)?;
            copy_from_actor(space, pointer, bytes)
        })?;
        deliver_message(
            destination,
            MessageMetadata {
                sender: address,
                tag,
                correlation,
            },
            bytes,
        )
    })();
    answer(
        trap_frame,
        result.err().unwrap_or(SystemCallStatus::Accepted),
        0,
    );
    trap_frame
}

/// Receives the oldest metadata match, preserving it on failed copy-out.
#[must_use]
pub fn receive_message(trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    let [pointer, capacity, flags, sender, tag, correlation] = arguments(trap_frame);
    let mut required_size = 0u64;
    let result = (|| {
        let selector = selector(flags, sender, tag, correlation)?;
        let address = caller()?;
        let selected = with_caller(address, |actor| {
            actor
                .inbox
                .as_mut()
                .unwrap_or_else(|| halt_machine())
                .receive_or_arm(selector)
        });
        let lease = match selected {
            ReceiveResult::Leased(lease) => lease,
            ReceiveResult::Armed(token) => {
                with_caller(address, |actor| {
                    if !actor
                        .inbox
                        .as_mut()
                        .unwrap_or_else(|| halt_machine())
                        .cancel_armed_wait(token)
                    {
                        halt_machine();
                    }
                });
                return Err(SystemCallStatus::WouldBlock);
            }
            ReceiveResult::Saturated | ReceiveResult::IdentifierExhausted => {
                return Err(SystemCallStatus::BudgetExhausted);
            }
            _ => return Err(SystemCallStatus::InvalidArgument),
        };
        let mut record = [0u8; MAXIMUM_RECORD_BYTE_COUNT];
        let metadata = lease.envelope().metadata;
        for (index, word) in [
            pack_local_address(metadata.sender),
            metadata.tag,
            metadata.correlation,
        ]
        .into_iter()
        .enumerate()
        {
            record
                .get_mut(index * 8..index * 8 + 8)
                .unwrap_or_else(|| halt_machine())
                .copy_from_slice(&word.to_le_bytes());
        }
        let copy_result = {
            let pool = MESSAGE_POOL.lock();
            let payload_length = pool
                .payload_length(&lease.envelope().descriptor)
                .unwrap_or_else(|_| halt_machine());
            let record_length = RECEIVED_HEADER_BYTE_COUNT + payload_length;
            required_size = record_length as u64;
            if capacity < required_size {
                Err(SystemCallStatus::BufferTooSmall)
            } else {
                let payload = record
                    .get_mut(RECEIVED_HEADER_BYTE_COUNT..record_length)
                    .unwrap_or_else(|| halt_machine());
                let _copied = pool
                    .copy_payload(&lease.envelope().descriptor, payload)
                    .unwrap_or_else(|_| halt_machine());
                Ok(record_length)
            }
        };
        finish_receive(address, lease, copy_result, pointer, &record)
    })();
    let status = result.err().unwrap_or(SystemCallStatus::Accepted);
    let value = if matches!(
        status,
        SystemCallStatus::Accepted | SystemCallStatus::BufferTooSmall
    ) {
        required_size
    } else {
        0
    };
    answer(trap_frame, status, value);
    trap_frame
}

/// Copies out and resolves a lease while preserving ownership on every failure.
fn finish_receive(
    address: LocalAddress,
    lease: crate::actors::inbox::ReceiveLease<
        crate::actors::message_pool::MessageHandle,
        LocalAddress,
    >,
    copy_result: Result<usize, SystemCallStatus>,
    pointer: u64,
    record: &[u8],
) -> Result<(), SystemCallStatus> {
    let (status, released) = with_caller(address, |actor| {
        let copy_result = copy_result.and_then(|length| {
            let pointer =
                usize::try_from(pointer).map_err(|_| SystemCallStatus::InvalidUserMemory)?;
            let space = actor
                .address_space
                .as_ref()
                .ok_or(SystemCallStatus::InvalidUserMemory)?;
            copy_to_actor(
                space,
                pointer,
                record
                    .get(..length)
                    .ok_or(SystemCallStatus::InvalidArgument)?,
            )
        });
        let inbox = actor.inbox.as_mut().unwrap_or_else(|| halt_machine());
        match copy_result {
            Ok(()) => {
                let completed = inbox
                    .complete_receive(lease)
                    .unwrap_or_else(|_| halt_machine());
                match completed {
                    CompleteResult::Delivered(envelope) => (Ok(()), Some(envelope.descriptor)),
                    CompleteResult::Discard(envelope) => (
                        Err(SystemCallStatus::InvalidArgument),
                        Some(envelope.descriptor),
                    ),
                }
            }
            Err(error) => {
                let cancelled = inbox
                    .cancel_receive(lease)
                    .unwrap_or_else(|_| halt_machine());
                let released = match cancelled {
                    CancelResult::Restored => None,
                    CancelResult::Discard(envelope) => Some(envelope.descriptor),
                };
                (Err(error), released)
            }
        }
    });
    // Self-exit cannot run until this syscall returns. Release the payload
    // before returning to EL0; forced remote exit requires an in-flight pin.
    if let Some(handle) = released {
        release_message(handle);
    }
    status
}

/// Waits without consuming; frame publication and the Waiting state are atomic.
#[must_use]
pub fn await_inbox(trap_frame: *mut TrapFrame) -> *mut TrapFrame {
    let [flags, sender, tag, correlation, timeout, reserved] = arguments(trap_frame);
    let preparation = (|| {
        if reserved != 0 || (timeout != 0 && timeout != INDEFINITE_WAIT) {
            return Err(SystemCallStatus::UnsupportedOperation);
        }
        Ok((caller()?, selector(flags, sender, tag, correlation)?))
    })();
    let (address, selector) = match preparation {
        Ok(prepared) => prepared,
        Err(status) => {
            answer(trap_frame, status, 0);
            return trap_frame;
        }
    };
    let core = current_core_local().unwrap_or_else(|| halt_machine());
    let parked = with_caller(address, |actor| {
        let inbox = actor.inbox.as_mut().unwrap_or_else(|| halt_machine());
        match inbox.await_matching(selector) {
            InboxWaitResult::Ready => {
                answer(trap_frame, SystemCallStatus::Accepted, 0);
                false
            }
            InboxWaitResult::Armed(token) => {
                if timeout == 0 {
                    if !inbox.cancel_armed_wait(token) {
                        halt_machine();
                    }
                    answer(trap_frame, SystemCallStatus::TimedOut, 0);
                    return false;
                }
                if inbox.finish_park(token) != ParkResult::Parked {
                    halt_machine();
                }
                answer(trap_frame, SystemCallStatus::Accepted, 0);
                actor.saved_frame_address = trap_frame as usize;
                if actor.queued {
                    halt_machine();
                }
                // No core may retain this actor's lower-half root once it can
                // wake elsewhere and exit. Detach before publishing Waiting.
                crate::architecture::aarch64::memory_management_unit::deactivate_lower_half();
                core.set_current_slot_index(NO_ACTOR);
                actor.state = ActorState::Waiting;
                true
            }
            InboxWaitResult::Saturated | InboxWaitResult::IdentifierExhausted => {
                answer(trap_frame, SystemCallStatus::BudgetExhausted, 0);
                false
            }
            _ => {
                answer(trap_frame, SystemCallStatus::InvalidArgument, 0);
                false
            }
        }
    });
    // Never inspect or write the old frame after releasing the actor lock:
    // a sender may already have made it runnable on another core.
    if parked {
        pick_next_after_wait(core) as *mut TrapFrame
    } else {
        trap_frame
    }
}
