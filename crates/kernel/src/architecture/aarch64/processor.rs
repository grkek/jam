//! Instructions that control the executing core.

/// Sleeps until an event arrives or an interrupt is taken.
///
/// Returns at once when an event is already pending.
pub fn wait_for_event() {
    // SAFETY: `wfe` only pauses the core until the next event.
    unsafe {
        core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
    }
}

/// Sets the event flag of every core, waking those sleeping in `wait_for_event`.
pub fn send_event() {
    // SAFETY: `sev` only signals an event.
    unsafe {
        core::arch::asm!("sev", options(nomem, nostack, preserves_flags));
    }
}
