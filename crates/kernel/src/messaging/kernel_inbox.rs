//! Nonblocking consumption for the kernel's polling console actor.
//!
//! No pool lock is nested inside an actor lock. Oversized messages are consumed
//! and discarded, so an untrusted sender cannot wedge the console with one record.
//! This helper is only used by the current EL1 actor, which cannot self-exit
//! during the operation; forced remote termination is not implemented.

use crate::{
    actors::{
        control_block::ActorState,
        inbox::{CompleteResult, MessageMetadata, ReceiveResult, ReceiveSelector},
        message_storage::MESSAGE_POOL,
        table::with_control_block,
    },
    architecture::aarch64::power_interface::halt_machine,
    scheduling::core_local::current_core_local,
};
use foundation::address::LocalAddress;

/// Copies and consumes one message from the current kernel actor, if available.
/// Returns `None` on an empty inbox or a discarded oversized payload.
pub fn receive(output: &mut [u8]) -> Option<(MessageMetadata<LocalAddress>, usize)> {
    let slot = current_core_local()?.current_slot_index();
    let selected = with_control_block(slot, |actor| {
        if actor.state != ActorState::Running {
            return None;
        }
        let inbox = actor.inbox.as_mut()?;
        match inbox.receive_or_arm(ReceiveSelector::any()) {
            ReceiveResult::Leased(lease) => Some(lease),
            ReceiveResult::Armed(token) => {
                if !inbox.cancel_armed_wait(token) {
                    halt_machine();
                }
                None
            }
            _ => None,
        }
    })??;
    let metadata = selected.envelope().metadata;
    let copied = {
        let pool = MESSAGE_POOL.lock();
        let length = pool
            .payload_length(&selected.envelope().descriptor)
            .unwrap_or_else(|_| halt_machine());
        if length > output.len() {
            None
        } else {
            Some(
                pool.copy_payload(&selected.envelope().descriptor, output)
                    .unwrap_or_else(|_| halt_machine()),
            )
        }
    };
    let released = with_control_block(slot, |actor| {
        let inbox = actor.inbox.as_mut().unwrap_or_else(|| halt_machine());
        match inbox
            .complete_receive(selected)
            .unwrap_or_else(|_| halt_machine())
        {
            CompleteResult::Delivered(envelope) | CompleteResult::Discard(envelope) => {
                envelope.descriptor
            }
        }
    })
    .unwrap_or_else(|| halt_machine());
    super::delivery::release_message(released);
    copied.map(|length| (metadata, length))
}
