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
    },
    error, information, warning,
};

/// How long the boot core waits for one core to report in, in spin iterations.
const ONLINE_WAIT_SPIN_LIMIT: u64 = 100_000_000;

/// Secondary cores that have reached Rust with the memory management unit on.
static ONLINE_CORE_COUNT: AtomicUsize = AtomicUsize::new(0);
/// The boot core's start time, so every core's log lines share one time base.
static BOOT_TICKS: AtomicU64 = AtomicU64::new(0);

/// Starts every core the device tree lists except the boot core, one at a time.
///
/// Each started core switches on the memory map at `translation_table_address`.
pub fn start_secondary_cores(
    console: &mut EarlyConsole,
    boot_ticks: u64,
    summary: &DeviceTreeSummary,
    translation_table_address: usize,
) {
    let Some(conduit) = summary.psci_conduit else {
        warning!(console, boot_ticks, "No power interface described by the device tree: staying on one core");
        return;
    };
    BOOT_TICKS.store(boot_ticks, Ordering::Release);

    for core_index in 1..summary.cpu_count {
        let status = start_secondary_core(conduit, core_index as u64, translation_table_address);
        if status != PowerStateCoordinationInterfaceStatus::Success {
            error!(console, boot_ticks, "Core {core_index}: start request refused: {status:?}");
            continue;
        }
        if wait_until_online(core_index) {
            information!(console, boot_ticks, "Core {core_index}: online");
        } else {
            error!(console, boot_ticks, "Core {core_index}: did not report in");
        }
    }
}

/// Waits until at least `expected_online_count` secondary cores have reported in.
fn wait_until_online(expected_online_count: usize) -> bool {
    for _ in 0..ONLINE_WAIT_SPIN_LIMIT {
        if ONLINE_CORE_COUNT.load(Ordering::Acquire) >= expected_online_count {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// First Rust code a secondary core runs. Called from `secondary_entry` in `boot_entry.s`.
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(core_index: usize, translation_table_address: usize) -> ! {
    install_exception_vectors();
    switch_on_memory_management_unit(translation_table_address);

    let boot_ticks = BOOT_TICKS.load(Ordering::Acquire);
    let mut console = EarlyConsole;
    information!(console, boot_ticks, "Core {core_index}: running Rust with the memory management unit on");

    ONLINE_CORE_COUNT.fetch_add(1, Ordering::Release);
    halt()
}
