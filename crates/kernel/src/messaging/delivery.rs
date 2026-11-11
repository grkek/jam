//! Local message admission using the existing inbox and shared payload pool.
//!
//! Pool and actor locks are never nested. A failed admission returns ownership
//! to this module for release; dead incarnations silently discard messages.

use crate::{
    actors::{
        control_block::State,
        inbox::{Envelope, MessageMetadata, SendEffect, SendError},
        message_pool::MessageHandle,
        message_storage::MESSAGE_POOL,
        table::with_control_block,
    },
    architecture::aarch64::{
        memory_management_unit::actor_slot_ceiling, power_interface::halt_machine,
    },
    scheduling::scheduler::{notify_polling_actor, wake_inbox_actor},
};
use foundation::{address::LocalAddress, system_call_status::SystemCallStatus};

/// Releases a message outside actor locks; pool corruption stops execution.
pub fn release_message(handle: MessageHandle) {
    if MESSAGE_POOL.lock().release(handle).is_err() {
        halt_machine();
    }
}

/// Delivers an already copied local payload with kernel-authenticated metadata.
///
/// # Errors
/// Reports malformed destinations or exhausted storage/admission budgets.
pub fn tell_actor(
    destination: LocalAddress,
    metadata: MessageMetadata<LocalAddress>,
    payload: &[u8],
) -> Result<(), SystemCallStatus> {
    let slot_index = destination.slot_index() as usize;
    if slot_index >= actor_slot_ceiling() {
        return Err(SystemCallStatus::MalformedAddress);
    }

    let live = with_control_block(slot_index, |actor| {
        actor.generation_counter == destination.generation_counter()
            && !matches!(actor.state, State::Free | State::Exiting)
    })
    .unwrap_or(false);

    if !live {
        return Err(SystemCallStatus::MalformedAddress);
    }

    let allocation = MESSAGE_POOL
        .lock()
        .allocate::<LocalAddress>(destination, payload, []);

    let (handle, charge) = allocation.map_err(|_| SystemCallStatus::BudgetExhausted)?;
    let mut outstanding = Some(Envelope::new(metadata, handle, charge));

    let result = with_control_block(slot_index, |actor| {
        if actor.generation_counter != destination.generation_counter()
            || matches!(actor.state, State::Free | State::Exiting)
        {
            return Err(SystemCallStatus::MalformedAddress);
        }

        let Some(inbox) = actor.inbox.as_mut() else {
            halt_machine();
        };

        let Some(envelope) = outstanding.take() else {
            halt_machine();
        };

        match inbox.send(envelope) {
            Ok(SendEffect::Enqueued) => {
                notify_polling_actor(actor);
                Ok(())
            }
            Ok(SendEffect::Wake(token)) => {
                wake_inbox_actor(actor, token);
                Ok(())
            }
            Err((error, envelope)) => {
                outstanding = Some(envelope);
                if error == SendError::Closed {
                    Ok(())
                } else {
                    Err(SystemCallStatus::BudgetExhausted)
                }
            }
        }
    })
    .unwrap_or(Ok(()));

    if let Some(envelope) = outstanding {
        release_message(envelope.descriptor);
    }

    result
}
