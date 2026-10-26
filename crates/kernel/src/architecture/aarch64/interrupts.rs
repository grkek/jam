//! Dispatches interrupts taken by the exception vectors.

use super::interrupt_controller::{
    TIMER_INTERRUPT_IDENTIFIER, acknowledge_interrupt, end_of_interrupt,
};
use crate::scheduling::scheduler::on_timer_tick;

/// Handles one interrupt and returns the address of the frame to resume.
///
/// Normally that is `interrupted_frame_address`. The scheduler may return another frame, which
/// switches the core to a different actor.
pub fn handle_interrupt(interrupted_frame_address: usize) -> usize {
    let Some(interrupt_identifier) = acknowledge_interrupt() else {
        return interrupted_frame_address;
    };
    let frame_to_resume = if interrupt_identifier == TIMER_INTERRUPT_IDENTIFIER {
        on_timer_tick(interrupted_frame_address)
    } else {
        interrupted_frame_address
    };
    end_of_interrupt(interrupt_identifier);
    frame_to_resume
}
