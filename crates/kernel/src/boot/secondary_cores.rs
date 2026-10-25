//! Brings the other cores out of power-off and into Rust.

use core::{
    fmt::Write,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use foundation::device_tree::DeviceTreeSummary;

use super::entry::halt;
use crate::{
    architecture::aarch64::{
        early_console::EarlyConsole,
        exceptions::install_exception_vectors,
        memory_management_unit::switch_on_memory_management_unit,
        power_interface::{PowerStateCoordinationInterfaceStatus, start_secondary_core},
        time,
    },
    error, information, warning,
};

const ONLINE_WAIT_TIMEOUT_IN_SECONDS: u64 = 1;

/// Secondary cores that have reached Rust with the memory management unit on.
static ONLINE_CORE_COUNT: AtomicUsize = AtomicUsize::new(0);
/// The boot core's start time, so every core's log lines share one time base.
static BOOT_TICKS: AtomicU64 = AtomicU64::new(0);

/// Starts every core the device tree lists except the boot core, all at once, and waits for them.
///
/// Each started core switches on the memory map at `translation_table_address`.
pub fn start_secondary_cores(
    console: &mut EarlyConsole,
    boot_ticks: u64,
    summary: &DeviceTreeSummary,
    translation_table_address: usize,
) {
    let Some(conduit) = summary.power_state_coordination_interface_conduit else {
        warning!(console, boot_ticks, "No power interface described by the device tree: staying on one core");
        return;
    };
    BOOT_TICKS.store(boot_ticks, Ordering::Release);

    let mut requested_core_count = 0;
    for core_index in 1..summary.central_processing_unit_count {
        let status = start_secondary_core(conduit, core_index as u64, translation_table_address);
        if status == PowerStateCoordinationInterfaceStatus::Success {
            requested_core_count += 1;
        } else {
            error!(console, boot_ticks, "Core {core_index}: start request refused: {status:?}");
        }
    }

    let timeout_ticks = time::read_counter_frequency() * ONLINE_WAIT_TIMEOUT_IN_SECONDS;
    let wait_started_ticks = time::read_counter_value();
    while ONLINE_CORE_COUNT.load(Ordering::Acquire) < requested_core_count
        && time::read_counter_value().wrapping_sub(wait_started_ticks) < timeout_ticks
    {
        core::hint::spin_loop();
    }

    let online_core_count = ONLINE_CORE_COUNT.load(Ordering::Acquire) + 1;
    
    if online_core_count == summary.central_processing_unit_count {
        information!(console, boot_ticks, "Multiprocessing initialized: {online_core_count} of {} cores online", summary.central_processing_unit_count);
    } else {
        warning!(console, boot_ticks, "Multiprocessing initialized partially: only {online_core_count} of {} cores online", summary.central_processing_unit_count);
    }
}

/// First Rust code a secondary core runs. Called from `secondary_entry` in `boot_entry.s`.
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(_core_index: usize, translation_table_address: usize) -> ! {
    install_exception_vectors();
    switch_on_memory_management_unit(translation_table_address);

    ONLINE_CORE_COUNT.fetch_add(1, Ordering::Release);
    halt();
}