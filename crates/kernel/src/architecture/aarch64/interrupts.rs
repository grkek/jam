//! Dispatches interrupts taken by the exception vectors.

use super::interrupt_controller::{
    TIMER_INTERRUPT_IDENTIFIER, acknowledge_interrupt, end_of_interrupt,
};
use crate::{
    architecture::aarch64::{early_console::EarlyConsole, exceptions::TrapFrame},
    error,
    scheduling::{
        core_local::{NO_ACTOR, current_core_local},
        scheduler::on_timer_tick,
    },
};

/// Handles one interrupt and returns the address of the frame to resume.
///
/// Normally that is `interrupted_frame_address`. The scheduler may return another frame, which
/// switches the core to a different actor.
#[must_use]
pub fn handle_interrupt(interrupted_frame_address: usize) -> usize {
    let interrupt_identifier = acknowledge_interrupt();

    let frame_to_resume = if let Some(identifier) = interrupt_identifier {
        let next_frame = if identifier == TIMER_INTERRUPT_IDENTIFIER {
            on_timer_tick(interrupted_frame_address)
        } else {
            interrupted_frame_address
        };

        end_of_interrupt(identifier);
        next_frame
    } else {
        interrupted_frame_address
    };

    if frame_to_resume != 0 {
        let frame = unsafe { &*(frame_to_resume as *const TrapFrame) };

        let slot = current_core_local()
            .map(|core| core.current_slot_index())
            .unwrap_or(NO_ACTOR);

        if frame.exception_link_register == 0
            || (slot == NO_ACTOR && (frame.saved_program_status_register & 0xf) != 5)
        {
            let incoming = unsafe { &*(interrupted_frame_address as *const TrapFrame) };

            error!(
                EarlyConsole,
                "IRQ INPUT: core={} frame={:#x} vector={} PC={:#x} SPSR={:#x} returned_frame={:#x}",
                current_core_local()
                    .map(|core| core.core_index())
                    .unwrap_or(usize::MAX),
                interrupted_frame_address,
                incoming.vector_index,
                incoming.exception_link_register,
                incoming.saved_program_status_register,
                frame_to_resume,
            );

            error!(
                EarlyConsole,
                "BAD IRQ: irq={:?} slot={} vector={} PC={:#x} SPSR={:#x} frame={:#x}",
                interrupt_identifier,
                slot,
                frame.vector_index,
                frame.exception_link_register,
                frame.saved_program_status_register,
                frame_to_resume,
            );
        }
    }

    frame_to_resume
}
