//! Nonblocking consumption for the kernel's polling console actor.
//!
//! No pool lock is nested inside an actor lock. Oversized messages are consumed
//! and discarded, so an untrusted sender cannot wedge the console with one record.
//! This helper is only used by the current EL1 actor, which cannot self-exit
//! during the operation; forced remote termination is not implemented.

use crate::{
    actors::{
        control_block::State,
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
        if actor.state != State::Running {
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

/// Sleeps only after advertising the polling wait and rechecking the inbox.
///
/// Local IRQs stay masked from publication through WFI. A producer either sees
/// the flag and sends an SGI, or publishes before our locked inbox check sees it.
/// A pending IRQ wakes WFI even while masked; restoring DAIF then handles it.
/// The periodic timer also wakes this polling driver to inspect UART input.
/// No spin lock is held while sleeping, and the actor cannot migrate in this span.
pub fn wait_for_request_or_interrupt() {
    use crate::architecture::aarch64::processor::{mask_interrupts, restore_interrupts};
    let saved_interrupt_state = mask_interrupts();
    let Some(core_local) = current_core_local() else {
        halt_machine();
    };
    core_local.set_polling_inbox(true);
    let has_messages = with_control_block(core_local.current_slot_index(), |actor| {
        actor
            .inbox
            .as_ref()
            .is_some_and(|inbox| inbox.message_count() != 0)
    })
    .unwrap_or(true);
    if !has_messages {
        // SAFETY: The polling handshake is visible, IRQs are masked, and all
        // locks have been released. The timer remains enabled as an input poll.
        unsafe {
            core::arch::asm!("dsb sy", "wfi", options(nostack, preserves_flags));
        }
    }
    core_local.set_polling_inbox(false);
    restore_interrupts(saved_interrupt_state);
}
