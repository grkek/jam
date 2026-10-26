//! The generic timer's EL1 physical timer, used as the scheduler tick.

const TIMER_CONTROL_ENABLE: u64 = 1;

/// Arms the timer to fire once, `interval_in_ticks` from now.
pub fn rearm_tick(interval_in_ticks: u64) {
    // SAFETY: Writing `CNTP_TVAL_EL0` only programs this core's own timer.
    unsafe {
        core::arch::asm!(
            "msr cntp_tval_el0, {interval}",
            "isb",
            interval = in(reg) interval_in_ticks,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Arms the timer and turns it on, with its interrupt unmasked.
pub fn start_tick(interval_in_ticks: u64) {
    rearm_tick(interval_in_ticks);
    // SAFETY: Writing `CNTP_CTL_EL0` only enables this core's own timer.
    unsafe {
        core::arch::asm!(
            "msr cntp_ctl_el0, {control}",
            "isb",
            control = in(reg) TIMER_CONTROL_ENABLE,
            options(nomem, nostack, preserves_flags),
        );
    }
}
