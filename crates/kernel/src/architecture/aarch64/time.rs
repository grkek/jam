//! Minimal uptime formatting for early boot logs.
//!
//! This is a boot aid, not a time service. The real time and logging pipelines belong to service actors.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

static BOOT_TICKS: AtomicU64 = AtomicU64::new(0);

/// Record and collect all of the boot ticks.
pub fn record_boot_ticks(ticks: u64) {
    BOOT_TICKS.store(ticks, Ordering::Relaxed);
}

/// Boot ticks which have been recorded.
#[must_use]
pub fn boot_ticks() -> u64 {
    BOOT_TICKS.load(Ordering::Relaxed)
}

/// Reads the architected timer frequency in ticks per second.
#[must_use]
pub fn read_counter_frequency() -> u64 {
    let freq: u64;
    unsafe {
        asm!("mrs {}, cntfrq_el0", out(reg) freq);
    }
    freq
}

/// Reads the current architected virtual counter value.
#[must_use]
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
    #[must_use]
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

/// Shared implementation: one timestamped line, written with a single `write_fmt`.
#[doc(hidden)]
#[macro_export]
macro_rules! log_line {
    ($console:expr, $($arg:tt)*) => {{
        use ::core::fmt::Write as _;
        let ts = $crate::architecture::aarch64::time::Timestamp::now(
            $crate::architecture::aarch64::time::boot_ticks(),
        );
        let _ = ::core::writeln!(
            $console,
            "[{:04}.{:04}] {}",
            ts.seconds,
            ts.fractional,
            ::core::format_args!($($arg)*)
        );
    }};
}

/// Information logging macro.
#[macro_export]
macro_rules! information {
    ($console:expr, $($arg:tt)*) => { $crate::log_line!($console, $($arg)*) };
}

/// Warning logging macro.
#[macro_export]
macro_rules! warning {
    ($console:expr, $($arg:tt)*) => { $crate::log_line!($console, $($arg)*) };
}

/// Error logging macro.
#[macro_export]
macro_rules! error {
    ($console:expr, $($arg:tt)*) => { $crate::log_line!($console, $($arg)*) };
}