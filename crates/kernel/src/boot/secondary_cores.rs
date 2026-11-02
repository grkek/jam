//! Brings the other cores out of power-off and into Rust.

use core::sync::atomic::{AtomicUsize, Ordering};

use foundation::device_tree::DeviceTreeSummary;

use crate::{
    architecture::aarch64::{
        early_console::EarlyConsole,
        exceptions, memory_management_unit,
        power_interface::{self, PowerStateCoordinationInterfaceStatus},
        processor, time,
    },
    error, information,
    scheduling::{core_local, scheduler},
    warning,
};

/// How long the boot core waits for the secondary cores to come online before giving up.
const ONLINE_WAIT_TIMEOUT_IN_SECONDS: u64 = 1;

/// Secondary cores that have reached Rust with the memory management unit on.
static ONLINE_CORE_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Starts every core the device tree lists except the boot core, all at once, and waits for them.
///
/// Each started core switches on the memory map at `translation_table_address`.
pub fn start_secondary_cores(summary: &DeviceTreeSummary, translation_table_address: usize) {
    let Some(conduit) = summary.power_state_coordination_interface_conduit else {
        warning!(
            EarlyConsole,
            "No power interface described by the device tree: staying on one core"
        );
        return;
    };

    let mut requested_core_count = 0;
    for core_index in 1..summary
        .central_processing_unit_count
        .min(core_local::CORE_MAXIMUM)
    {
        let status = power_interface::start_secondary_core(
            conduit,
            core_index as u64,
            translation_table_address,
        );
        if status == PowerStateCoordinationInterfaceStatus::Success {
            requested_core_count += 1;
        } else {
            error!(
                EarlyConsole,
                "Core {core_index}: start request refused: {status:?}"
            );
        }
    }

    let timeout_ticks = time::read_counter_frequency() * ONLINE_WAIT_TIMEOUT_IN_SECONDS;
    let wait_started_ticks = time::read_counter_value();
    while ONLINE_CORE_COUNT.load(Ordering::Acquire) < requested_core_count
        && time::read_counter_value().wrapping_sub(wait_started_ticks) < timeout_ticks
    {
        // Sleep in low-power state until an event (sev) wakes us up,
        // or the check loop re-evaluates on timer ticks/spurious wakeups.
        processor::wait_for_event();
    }

    let online_core_count = ONLINE_CORE_COUNT.load(Ordering::Acquire) + 1;

    if online_core_count == summary.central_processing_unit_count {
        information!(
            EarlyConsole,
            "Multiprocessing initialised: {online_core_count} of {} cores online",
            summary.central_processing_unit_count
        );
    } else {
        warning!(
            EarlyConsole,
            "Multiprocessing initialised partially: only {online_core_count} of {} cores online",
            summary.central_processing_unit_count
        );
    }
}

/// First Rust code a secondary core runs. Called from `secondary_entry` in `boot_entry.s`.
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(core_index: usize, translation_table_address: usize) -> ! {
    exceptions::install_exception_vectors();
    // Interrupt and fault handlers pick this core's stack from TPIDR_EL1, so set it before
    // anything can trap. No atomics yet: the memory management unit is still off.
    processor::write_current_core_index(core_index);

    memory_management_unit::switch_kernel_translation_root(translation_table_address);

    if !core_local::register_current_core(core_index) {
        // This kernel has no room for a core with this index.
        loop {
            processor::wait_for_event();
        }
    }

    ONLINE_CORE_COUNT.fetch_add(1, Ordering::Release);

    // Wake the boot core, which may be waiting for this core to arrive.
    processor::send_event();

    scheduler::start_current_core_tick();

    loop {
        processor::wait_for_event();
    }
}
