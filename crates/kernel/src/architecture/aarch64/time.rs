//! Minimal uptime formatting for early boot logs.
//!
//! This is a boot aid, not a time service. The real time and logging pipelines belong to service actors.

use core::arch::asm;

/// Reads the architected timer frequency in ticks per second.
pub fn read_counter_frequency() -> u64 {
    let freq: u64;
    unsafe {
        asm!("mrs {}, cntfrq_el0", out(reg) freq);
    }
    freq
}

/// Reads the current architected virtual counter value.
pub fn read_counter_value() -> u64 {
    let val: u64;
    unsafe {
        asm!("mrs {}, cntvct_el0", out(reg) val);
    }
    val
}

/// Elapsed uptime represented as whole seconds and ten-thousandths of a second.
pub struct Timestamp {
    /// The number of whole elapsed seconds.
    pub seconds: u64,
    /// The fractional portion of the elapsed second, in ten-thousandths.
    pub fractional: u64,
}

impl Timestamp {
    /// Calculates elapsed uptime from the virtual counter's boot baseline.
    pub fn now(boot_ticks: u64) -> Self {
        let freq = read_counter_frequency();
        let current = read_counter_value();
        let elapsed_ticks = current.saturating_sub(boot_ticks);
        let seconds = elapsed_ticks / freq;
        let remainder = elapsed_ticks % freq;
        let fractional = (remainder * 10000) / freq;
        Self {
            seconds,
            fractional,
        }
    }
}

/// Early boot information logging macro.
#[macro_export]
macro_rules! information {
    ($console:expr, $boot_ticks:expr, $($arg:tt)*) => {{
        let ts = $crate::architecture::aarch64::time::Timestamp::now($boot_ticks);
        let _ = write!($console, "[{:04}.{:04}] ", ts.seconds, ts.fractional);
        let _ = writeln!($console, $($arg)*);
    }};
}

/// Early boot warning logging macro.
#[macro_export]
macro_rules! warning {
    ($console:expr, $boot_ticks:expr, $($arg:tt)*) => {{
        let ts = $crate::architecture::aarch64::time::Timestamp::now($boot_ticks);
        let _ = write!($console, "[{:04}.{:04}] ", ts.seconds, ts.fractional);
        let _ = writeln!($console, $($arg)*);
    }};
}

/// Early boot error logging macro.
#[macro_export]
macro_rules! error {
    ($console:expr, $boot_ticks:expr, $($arg:tt)*) => {{
        let ts = $crate::architecture::aarch64::time::Timestamp::now($boot_ticks);
        let _ = write!($console, "[{:04}.{:04}] ", ts.seconds, ts.fractional);
        let _ = writeln!($console, $($arg)*);
    }};
}
